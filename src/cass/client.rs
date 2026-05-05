//! High-level handle for the CASS CLI subprocess (EE-100, EE-101).
//!
//! `CassClient` is the thin facade `ee` core code uses to talk to the
//! installed `cass` binary. It owns the binary path, the static set of
//! environment overrides we always apply (per the contract-stability
//! spike), and the CLI surface for building [`CassInvocation`]s.
//!
//! EE-101 adds binary discovery: [`discover`] searches `$PATH` for `cass`,
//! [`discover_with_override`] accepts an explicit config path, and both
//! return a [`DiscoveredBinary`] with provenance for diagnostics.
//!
//! What this slice deliberately does **not** do:
//!
//! * parse JSON — the bead title is "Add `cass` module", not
//!   "implement the full preflight";
//! * execute the preflight — [`CassClient::preflight_invocations`]
//!   returns the *invocations* the next bead will run, so we ship a
//!   testable contract today;
//! * cache results — caching has its own bead and would prejudge the
//!   shape of the durable side.
//!
//! Future work plugs a JSON parser and a contract cache in behind the
//! types defined here.

use std::ffi::{OsStr, OsString};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::error::CassError;
use super::process::CassInvocation;

/// Default binary name `ee` resolves through `$PATH` when the config
/// does not pin an explicit location.
pub const DEFAULT_BINARY: &str = "cass";
/// Default wall-clock budget for one CASS subprocess call.
pub const DEFAULT_SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(30);

/// How the CASS binary was located (EE-101).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoverySource {
    /// Found via `$PATH` lookup.
    Path,
    /// Explicit path from `[cass.binary]` config.
    Config,
    /// Explicit path from `EE_CASS_BINARY` environment variable.
    EnvVar,
}

impl DiscoverySource {
    /// Stable lowercase tag for JSON status output and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Config => "config",
            Self::EnvVar => "env_var",
        }
    }
}

/// Result of CASS binary discovery (EE-101).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredBinary {
    /// Absolute path to the discovered binary.
    pub path: PathBuf,
    /// How the binary was located.
    pub source: DiscoverySource,
}

impl DiscoveredBinary {
    /// Create a new discovery result.
    #[must_use]
    pub fn new(path: PathBuf, source: DiscoverySource) -> Self {
        Self { path, source }
    }
}

/// Discover the CASS binary by searching `$PATH` for `cass`.
///
/// Returns the first executable `cass` found in `$PATH`, or
/// [`CassError::BinaryNotFound`] if none exists.
///
/// # Errors
///
/// Returns [`CassError::BinaryNotFound`] if `cass` is not in `$PATH`.
pub fn discover() -> Result<DiscoveredBinary, CassError> {
    discover_with_override(None)
}

/// Discover the CASS binary with an optional explicit override.
///
/// Priority order:
/// 1. `EE_CASS_BINARY` environment variable (if set)
/// 2. `config_override` parameter (if `Some`)
/// 3. `$PATH` lookup for `cass`
///
/// # Errors
///
/// Returns [`CassError::BinaryNotFound`] if no binary is found.
/// Returns [`CassError::InvalidBinary`] if an override path does not exist.
pub fn discover_with_override(
    config_override: Option<&Path>,
) -> Result<DiscoveredBinary, CassError> {
    // Check EE_CASS_BINARY env var first
    if let Ok(env_path) = std::env::var("EE_CASS_BINARY") {
        let path = PathBuf::from(&env_path);
        if path.is_file() {
            return Ok(DiscoveredBinary::new(
                canonicalize_path(&path)?,
                DiscoverySource::EnvVar,
            ));
        }
        return Err(CassError::InvalidBinary {
            binary: path,
            reason: "EE_CASS_BINARY path does not exist or is not a file".to_string(),
        });
    }

    // Check config override
    if let Some(override_path) = config_override {
        if override_path.is_file() {
            return Ok(DiscoveredBinary::new(
                canonicalize_path(override_path)?,
                DiscoverySource::Config,
            ));
        }
        return Err(CassError::InvalidBinary {
            binary: override_path.to_path_buf(),
            reason: "config [cass.binary] path does not exist or is not a file".to_string(),
        });
    }

    // Search $PATH
    if let Some(path) = search_path_for(DEFAULT_BINARY) {
        return Ok(DiscoveredBinary::new(path, DiscoverySource::Path));
    }

    Err(CassError::BinaryNotFound {
        binary: PathBuf::from(DEFAULT_BINARY),
    })
}

/// Discover the CASS binary for production import execution without
/// trusting the caller's inherited `$PATH`.
///
/// Priority order:
/// 1. `EE_CASS_BINARY`, if set, as an absolute executable path
/// 2. explicit config override, if it is not the built-in `cass` default
/// 3. known installation locations
///
/// # Errors
///
/// Returns [`CassError::BinaryNotFound`] when no trusted location contains
/// `cass`, or [`CassError::InvalidBinary`] when an explicit override is not an
/// absolute, executable `cass` file.
pub fn discover_import_binary(
    config_override: Option<&Path>,
) -> Result<DiscoveredBinary, CassError> {
    let env_override = std::env::var_os("EE_CASS_BINARY");
    discover_import_binary_from_sources(
        env_override.as_deref(),
        config_override,
        &trusted_cass_locations(),
    )
}

fn discover_import_binary_from_sources(
    env_override: Option<&OsStr>,
    config_override: Option<&Path>,
    trusted_locations: &[PathBuf],
) -> Result<DiscoveredBinary, CassError> {
    if let Some(env_path) = env_override {
        let path = PathBuf::from(env_path);
        return validate_import_binary(&path, DiscoverySource::EnvVar);
    }

    if let Some(override_path) = config_override {
        if override_path != Path::new(DEFAULT_BINARY) {
            return validate_import_binary(override_path, DiscoverySource::Config);
        }
    }

    for candidate in trusted_locations {
        if candidate.is_file() {
            return validate_import_binary(candidate, DiscoverySource::Path);
        }
    }

    Err(CassError::BinaryNotFound {
        binary: PathBuf::from(DEFAULT_BINARY),
    })
}

fn trusted_cass_locations() -> Vec<PathBuf> {
    let mut locations = vec![
        PathBuf::from("/usr/local/bin/cass"),
        PathBuf::from("/usr/bin/cass"),
        PathBuf::from("/opt/homebrew/bin/cass"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        locations.push(PathBuf::from(home).join(".local/bin/cass"));
    }
    locations
}

fn validate_import_binary(
    path: &Path,
    source: DiscoverySource,
) -> Result<DiscoveredBinary, CassError> {
    if !path.is_absolute() {
        return Err(CassError::InvalidBinary {
            binary: path.to_path_buf(),
            reason: "CASS import binary must be configured as an absolute path".to_string(),
        });
    }
    if path.file_name() != Some(OsStr::new(DEFAULT_BINARY)) {
        return Err(CassError::InvalidBinary {
            binary: path.to_path_buf(),
            reason: "CASS import binary file name must be `cass`".to_string(),
        });
    }
    validate_import_binary_metadata(path)?;
    Ok(DiscoveredBinary::new(canonicalize_path(path)?, source))
}

#[cfg(unix)]
fn validate_import_binary_metadata(path: &Path) -> Result<(), CassError> {
    let metadata = fs::metadata(path).map_err(|error| CassError::InvalidBinary {
        binary: path.to_path_buf(),
        reason: format!("CASS import binary metadata is unavailable: {error}"),
    })?;
    if !metadata.is_file() {
        return Err(CassError::InvalidBinary {
            binary: path.to_path_buf(),
            reason: "CASS import binary path is not a file".to_string(),
        });
    }
    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 {
        return Err(CassError::InvalidBinary {
            binary: path.to_path_buf(),
            reason: "CASS import binary is not executable".to_string(),
        });
    }
    if mode & 0o022 != 0 {
        return Err(CassError::InvalidBinary {
            binary: path.to_path_buf(),
            reason: "CASS import binary must not be writable by group or other".to_string(),
        });
    }
    if let Some(parent) = path.parent() {
        let parent_metadata = fs::metadata(parent).map_err(|error| CassError::InvalidBinary {
            binary: path.to_path_buf(),
            reason: format!("CASS import binary parent metadata is unavailable: {error}"),
        })?;
        if parent_metadata.permissions().mode() & 0o002 != 0 {
            return Err(CassError::InvalidBinary {
                binary: path.to_path_buf(),
                reason: "CASS import binary parent directory must not be writable by other"
                    .to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_import_binary_metadata(path: &Path) -> Result<(), CassError> {
    let metadata = fs::metadata(path).map_err(|error| CassError::InvalidBinary {
        binary: path.to_path_buf(),
        reason: format!("CASS import binary metadata is unavailable: {error}"),
    })?;
    if metadata.is_file() {
        Ok(())
    } else {
        Err(CassError::InvalidBinary {
            binary: path.to_path_buf(),
            reason: "CASS import binary path is not a file".to_string(),
        })
    }
}

/// Search `$PATH` for the named binary and return the first match.
fn search_path_for(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Canonicalize a path, mapping I/O errors to CassError.
fn canonicalize_path(path: &Path) -> Result<PathBuf, CassError> {
    path.canonicalize().map_err(|e| CassError::Io {
        message: format!("failed to canonicalize {}: {}", path.display(), e),
    })
}

/// Stable environment overrides `ee` applies on every CASS subprocess.
///
/// These come straight out of the contract-stability spike and are
/// intentionally narrow:
///
/// * `CASS_IGNORE_SOURCES_CONFIG=1` — pins source discovery so two
///   adjacent `ee` runs see the same CASS index regardless of
///   `~/.config/cass/sources.toml` drift.
/// * `CODING_AGENT_SEARCH_NO_UPDATE_PROMPT=1` — disables the
///   interactive update prompt so headless invocations cannot block.
///
/// Order is preserved: tests assert exact ordering so audit logs are
/// byte-stable.
pub const STABLE_ENV_OVERRIDES: &[(&str, &str)] = &[
    ("CASS_IGNORE_SOURCES_CONFIG", "1"),
    ("CODING_AGENT_SEARCH_NO_UPDATE_PROMPT", "1"),
];

/// Configuration handle for the CASS adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassClient {
    binary: PathBuf,
    extra_env: Vec<(OsString, OsString)>,
    subprocess_timeout: Duration,
}

impl CassClient {
    /// Build a client that resolves the default `cass` binary through
    /// `$PATH`.
    #[must_use]
    pub fn new_default() -> Self {
        Self::with_binary(DEFAULT_BINARY)
    }

    /// Build a client from a discovered binary (EE-101).
    ///
    /// This is the preferred constructor after discovery: it records the
    /// absolute path so invocations bypass the allowlist check and run
    /// the validated binary directly.
    #[must_use]
    pub fn from_discovered(discovered: DiscoveredBinary) -> Self {
        Self {
            binary: discovered.path,
            extra_env: Vec::new(),
            subprocess_timeout: DEFAULT_SUBPROCESS_TIMEOUT,
        }
    }

    /// Build a client that records `binary` in the invocation intent.
    ///
    /// For production use, prefer [`discover`] + [`Self::from_discovered`]
    /// which validates the binary exists. This constructor is useful for
    /// tests and provenance fixtures.
    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            extra_env: Vec::new(),
            subprocess_timeout: DEFAULT_SUBPROCESS_TIMEOUT,
        }
    }

    /// Append an extra environment override to every subsequent
    /// invocation. The stable spike-mandated overrides are still
    /// applied first; user-supplied values appended here win on key
    /// collision (matching `Command::env`).
    #[must_use]
    pub fn with_extra_env<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<OsString>,
        V: Into<OsString>,
    {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    /// Override the wall-clock budget applied to every CASS subprocess.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.subprocess_timeout = timeout;
        self
    }

    /// Path the client will spawn.
    #[must_use]
    pub fn binary(&self) -> &Path {
        self.binary.as_path()
    }

    /// User-supplied environment overrides, in registration order.
    #[must_use]
    pub fn extra_env(&self) -> &[(OsString, OsString)] {
        self.extra_env.as_slice()
    }

    /// Wall-clock budget applied to every invocation produced by this client.
    #[must_use]
    pub const fn subprocess_timeout(&self) -> Duration {
        self.subprocess_timeout
    }

    /// Build a single [`CassInvocation`] for `cass <args...>`. The
    /// stable env overrides are always applied; per-call user env adds
    /// to them.
    pub fn invocation<I, S>(&self, args: I) -> CassInvocation
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut inv =
            CassInvocation::new(self.binary.clone(), args).with_timeout(self.subprocess_timeout);
        for (key, value) in STABLE_ENV_OVERRIDES {
            inv = inv.with_env(*key, *value);
        }
        for (key, value) in &self.extra_env {
            inv = inv.with_env(key.clone(), value.clone());
        }
        inv
    }

    /// Build an import-only invocation after proving the binary is an
    /// absolute, validated `cass` executable.
    ///
    /// Import reads arbitrary session content and may run from agent hooks, so
    /// it must never fall back to inherited `$PATH` lookup. Callers should
    /// construct import clients with [`discover_import_binary`] plus
    /// [`Self::from_discovered`].
    pub(crate) fn import_invocation<I, S>(&self, args: I) -> Result<CassInvocation, CassError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let binary = self.validated_import_binary()?;
        let mut inv = CassInvocation::new(binary, args).with_timeout(self.subprocess_timeout);
        for (key, value) in STABLE_ENV_OVERRIDES {
            inv = inv.with_env(*key, *value);
        }
        for (key, value) in &self.extra_env {
            inv = inv.with_env(key.clone(), value.clone());
        }
        Ok(inv)
    }

    fn validated_import_binary(&self) -> Result<PathBuf, CassError> {
        if self.binary == Path::new(DEFAULT_BINARY) {
            return Err(CassError::InvalidBinary {
                binary: self.binary.clone(),
                reason: "CASS import requires an absolute discovered binary; inherited PATH lookup is not allowed"
                    .to_string(),
            });
        }
        validate_import_binary(&self.binary, DiscoverySource::Config).map(|binary| binary.path)
    }

    /// Build the invocations the preflight bead (the slice that lands
    /// after EE-100) will run. Returning a vec of intent here lets us
    /// unit-test the exact arg list `ee` will hand to `cass` without
    /// spawning the binary.
    ///
    /// The current set is `cass api-version --json`,
    /// `cass capabilities --json`, and `cass introspect --json`, all
    /// of which are schema-backed in CASS's own golden suite.
    #[must_use]
    pub fn preflight_invocations(&self) -> Vec<CassInvocation> {
        vec![
            self.invocation(["api-version", "--json"]),
            self.invocation(["capabilities", "--json"]),
            self.invocation(["introspect", "--json"]),
        ]
    }

    /// Build a single search invocation for the given query.
    ///
    /// `ee` standardises on the spike's recommended search flag set:
    /// `--robot --robot-meta --fields minimal --max-tokens`. The
    /// `request_id` is echoed by CASS so callers can correlate stdout,
    /// stderr, and the `ee` audit log; we require the caller to provide
    /// it rather than generating one here, because deterministic IDs
    /// are how the pack-stability tests stay reproducible.
    pub fn search_invocation(
        &self,
        query: &str,
        request_id: &str,
        limit: u32,
        max_tokens: u32,
    ) -> CassInvocation {
        let timeout_ms = self.subprocess_timeout.as_millis().to_string();
        self.invocation([
            "search".to_owned(),
            query.to_owned(),
            "--robot".to_owned(),
            "--robot-meta".to_owned(),
            "--fields".to_owned(),
            "minimal".to_owned(),
            "--limit".to_owned(),
            limit.to_string(),
            "--max-tokens".to_owned(),
            max_tokens.to_string(),
            "--timeout".to_owned(),
            timeout_ms,
            "--request-id".to_owned(),
            request_id.to_owned(),
        ])
    }

    /// Build a `cass sessions --json` invocation for import discovery.
    pub fn sessions_invocation(&self, workspace_path: &Path, limit: u32) -> CassInvocation {
        self.invocation([
            "sessions".to_owned(),
            "--workspace".to_owned(),
            workspace_path.to_string_lossy().into_owned(),
            "--json".to_owned(),
            "--limit".to_owned(),
            limit.to_string(),
        ])
    }

    /// Build an import-safe `cass sessions --json` invocation.
    pub(crate) fn import_sessions_invocation(
        &self,
        workspace_path: &Path,
        limit: u32,
    ) -> Result<CassInvocation, CassError> {
        self.import_invocation([
            "sessions".to_owned(),
            "--workspace".to_owned(),
            workspace_path.to_string_lossy().into_owned(),
            "--json".to_owned(),
            "--limit".to_owned(),
            limit.to_string(),
        ])
    }

    /// Build a `cass view -n <line> -C <context> --json -- <path>` invocation.
    pub fn view_invocation(&self, source_path: &str, line: u32, context: u32) -> CassInvocation {
        self.invocation([
            "view".to_owned(),
            "-n".to_owned(),
            line.to_string(),
            "-C".to_owned(),
            context.to_string(),
            "--json".to_owned(),
            "--".to_owned(),
            source_path.to_owned(),
        ])
    }

    /// Build an import-safe `cass view -n <line> -C <context> --json -- <path>` invocation.
    pub(crate) fn import_view_invocation(
        &self,
        source_path: &str,
        line: u32,
        context: u32,
    ) -> Result<CassInvocation, CassError> {
        self.import_invocation([
            "view".to_owned(),
            "-n".to_owned(),
            line.to_string(),
            "-C".to_owned(),
            context.to_string(),
            "--json".to_owned(),
            "--".to_owned(),
            source_path.to_owned(),
        ])
    }

    /// Build a `cass expand -n <line> -C <context> --json -- <path>` invocation.
    pub fn expand_invocation(&self, source_path: &str, line: u32, context: u32) -> CassInvocation {
        self.invocation([
            "expand".to_owned(),
            "-n".to_owned(),
            line.to_string(),
            "-C".to_owned(),
            context.to_string(),
            "--json".to_owned(),
            "--".to_owned(),
            source_path.to_owned(),
        ])
    }

    /// Run the supplied invocation and translate spawn errors into
    /// the [`CassError`] taxonomy.
    ///
    /// # Errors
    ///
    /// Propagates the same set as [`CassInvocation::run`]:
    /// [`CassError::InvalidBinary`] for non-allowlisted executable
    /// paths, [`CassError::BinaryNotFound`] for missing `cass`, and
    /// [`CassError::Io`] for any other spawn failure.
    pub fn run(
        &self,
        invocation: &CassInvocation,
    ) -> Result<super::process::CassOutcome, CassError> {
        invocation.run()
    }
}

impl Default for CassClient {
    fn default() -> Self {
        Self::new_default()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    #[cfg(unix)]
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        CassClient, DEFAULT_BINARY, DiscoveredBinary, DiscoverySource, STABLE_ENV_OVERRIDES,
        discover, discover_import_binary_from_sources, discover_with_override,
    };

    type TestResult = Result<(), String>;

    #[cfg(unix)]
    fn unique_test_dir(prefix: &str) -> TestResultWith<PathBuf> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock moved backwards: {error}"))?
            .as_nanos();
        let target_dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
        Ok(target_dir
            .join("ee-cass-client-tests")
            .join(format!("{prefix}-{}-{now}", std::process::id())))
    }

    #[cfg(unix)]
    type TestResultWith<T> = Result<T, String>;

    #[cfg(unix)]
    fn write_test_cass_binary(path: &Path, mode: u32) -> TestResult {
        fs::write(path, "#!/bin/sh\nprintf '{\"ok\":true}\\n'\n")
            .map_err(|error| error.to_string())?;
        let mut permissions = fs::metadata(path)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions).map_err(|error| error.to_string())
    }

    #[test]
    fn new_default_uses_path_resolution() {
        let client = CassClient::new_default();
        assert_eq!(client.binary(), Path::new(DEFAULT_BINARY));
        assert!(client.extra_env().is_empty());
    }

    #[test]
    fn invocation_applies_stable_env_overrides_in_order() {
        let client = CassClient::new_default();
        let inv = client.invocation(["health", "--json"]);

        let env = inv.env_overrides();
        assert_eq!(env.len(), STABLE_ENV_OVERRIDES.len());
        for (i, (expected_key, expected_value)) in STABLE_ENV_OVERRIDES.iter().enumerate() {
            assert_eq!(env[i].0, *expected_key);
            assert_eq!(env[i].1, *expected_value);
        }
        assert_eq!(inv.binary(), Path::new(DEFAULT_BINARY));
        assert_eq!(inv.args(), ["health", "--json"]);
        assert_eq!(inv.timeout(), Some(super::DEFAULT_SUBPROCESS_TIMEOUT));
    }

    #[test]
    fn extra_env_appends_after_stable_overrides() -> TestResult {
        let client = CassClient::new_default().with_extra_env("EE_TRACE", "1");
        let inv = client.invocation(["health"]);
        let env = inv.env_overrides();
        assert_eq!(env.len(), STABLE_ENV_OVERRIDES.len() + 1);
        let last = env
            .last()
            .ok_or_else(|| "expected appended env override".to_string())?;
        assert_eq!(last.0, "EE_TRACE");
        assert_eq!(last.1, "1");
        Ok(())
    }

    #[test]
    fn preflight_invocations_target_schema_backed_surfaces_only() {
        let client = CassClient::new_default();
        let invs = client.preflight_invocations();
        assert_eq!(invs.len(), 3);
        assert_eq!(invs[0].args(), ["api-version", "--json"]);
        assert_eq!(invs[1].args(), ["capabilities", "--json"]);
        assert_eq!(invs[2].args(), ["introspect", "--json"]);
    }

    #[test]
    fn search_invocation_uses_recommended_flag_set() -> TestResult {
        let client = CassClient::new_default();
        let inv = client.search_invocation("rust", "ee-test-001", 5, 4000);

        let args: Result<Vec<&str>, String> = inv
            .args()
            .iter()
            .map(|os| match os.to_str() {
                Some(s) => Ok(s),
                None => Err("test arg must be ascii".to_string()),
            })
            .collect();
        let args = args?;

        assert_eq!(
            args,
            vec![
                "search",
                "rust",
                "--robot",
                "--robot-meta",
                "--fields",
                "minimal",
                "--limit",
                "5",
                "--max-tokens",
                "4000",
                "--timeout",
                "30000",
                "--request-id",
                "ee-test-001",
            ],
        );
        Ok(())
    }

    #[test]
    fn binary_path_is_round_trippable_through_with_binary() {
        let client = CassClient::with_binary("/opt/cass/bin/cass");
        assert_eq!(client.binary(), Path::new("/opt/cass/bin/cass"));
    }

    #[test]
    fn run_rejects_non_existent_binary() -> TestResult {
        let client = CassClient::with_binary("/no/such/cass-binary-eeplaceholder");
        let inv = client.invocation(["health", "--json"]);
        let error = match client.run(&inv) {
            Ok(_) => return Err("non-existent binary should fail".to_string()),
            Err(error) => error,
        };
        assert_eq!(error.kind_str(), "invalid_binary");
        Ok(())
    }

    #[test]
    fn discovery_source_strings_are_stable() {
        assert_eq!(DiscoverySource::Path.as_str(), "path");
        assert_eq!(DiscoverySource::Config.as_str(), "config");
        assert_eq!(DiscoverySource::EnvVar.as_str(), "env_var");
    }

    #[test]
    fn discover_finds_cass_in_path() {
        // This test only passes if cass is installed
        match discover() {
            Ok(discovered) => {
                assert!(discovered.path.is_absolute());
                assert!(discovered.path.is_file());
                assert_eq!(discovered.source, DiscoverySource::Path);
            }
            Err(e) => {
                // cass not installed is acceptable in test env
                assert_eq!(e.kind_str(), "binary_not_found");
            }
        }
    }

    #[test]
    fn discover_with_override_rejects_missing_config_path() -> TestResult {
        let result = discover_with_override(Some(Path::new("/no/such/cass-config-path")));
        let error = match result {
            Ok(_) => return Err("missing config path should fail".to_string()),
            Err(e) => e,
        };
        assert_eq!(error.kind_str(), "invalid_binary");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn import_discovery_ignores_inherited_path_only_cass() -> TestResult {
        let dir = unique_test_dir("path-ignored")?;
        let fake_dir = dir.join("fake-path");
        fs::create_dir_all(&fake_dir).map_err(|error| error.to_string())?;
        write_test_cass_binary(&fake_dir.join(DEFAULT_BINARY), 0o755)?;

        let result = discover_import_binary_from_sources(None, None, &[]);
        let error = match result {
            Ok(discovered) => {
                return Err(format!(
                    "inherited PATH must not produce import binary; got {}",
                    discovered.path.display()
                ));
            }
            Err(error) => error,
        };

        assert_eq!(error.kind_str(), "binary_not_found");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn import_discovery_accepts_explicit_absolute_env_binary() -> TestResult {
        let dir = unique_test_dir("env-binary")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join(DEFAULT_BINARY);
        write_test_cass_binary(&binary, 0o755)?;

        let discovered = discover_import_binary_from_sources(Some(binary.as_os_str()), None, &[])
            .map_err(|error| error.to_string())?;

        assert_eq!(discovered.source, DiscoverySource::EnvVar);
        assert_eq!(
            discovered.path,
            binary.canonicalize().map_err(|e| e.to_string())?
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn import_discovery_rejects_group_or_world_writable_binary() -> TestResult {
        let dir = unique_test_dir("writable-binary")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join(DEFAULT_BINARY);
        write_test_cass_binary(&binary, 0o777)?;

        let result = discover_import_binary_from_sources(Some(binary.as_os_str()), None, &[]);
        let error = match result {
            Ok(_) => return Err("world-writable cass binary should be rejected".to_string()),
            Err(error) => error,
        };

        assert_eq!(error.kind_str(), "invalid_binary");
        assert!(
            error.to_string().contains("writable by group or other"),
            "unexpected error: {error}",
        );
        Ok(())
    }

    #[test]
    fn from_discovered_creates_client_with_absolute_path() {
        let discovered = DiscoveredBinary::new(
            Path::new("/usr/bin/cass").to_path_buf(),
            DiscoverySource::Path,
        );
        let client = CassClient::from_discovered(discovered);
        assert_eq!(client.binary(), Path::new("/usr/bin/cass"));
    }

    #[test]
    fn view_expand_and_sessions_invocations_are_machine_readable() -> TestResult {
        let client = CassClient::new_default();

        let sessions = client.sessions_invocation(Path::new("/work"), 7);
        assert_eq!(
            sessions.args(),
            ["sessions", "--workspace", "/work", "--json", "--limit", "7"]
        );

        let view = client.view_invocation("/work/session.jsonl", 42, 4);
        assert_eq!(
            view.args(),
            [
                "view",
                "-n",
                "42",
                "-C",
                "4",
                "--json",
                "--",
                "/work/session.jsonl"
            ]
        );

        let expand = client.expand_invocation("/work/session.jsonl", 42, 3);
        assert_eq!(
            expand.args(),
            [
                "expand",
                "-n",
                "42",
                "-C",
                "3",
                "--json",
                "--",
                "/work/session.jsonl"
            ]
        );
        Ok(())
    }

    #[test]
    fn view_and_expand_invocations_separate_malicious_prefix_paths() {
        let client = CassClient::new_default();

        let view = client.view_invocation("--config=/tmp/evil", 42, 4);
        assert_eq!(
            view.args(),
            [
                "view",
                "-n",
                "42",
                "-C",
                "4",
                "--json",
                "--",
                "--config=/tmp/evil"
            ]
        );

        let expand = client.expand_invocation("-n", 42, 4);
        assert_eq!(
            expand.args(),
            ["expand", "-n", "42", "-C", "4", "--json", "--", "-n"]
        );
    }
}
