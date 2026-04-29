//! High-level handle for the CASS CLI subprocess (EE-100).
//!
//! `CassClient` is the thin facade `ee` core code uses to talk to the
//! installed `cass` binary. It owns the binary path, the static set of
//! environment overrides we always apply (per the contract-stability
//! spike), and the CLI surface for building [`CassInvocation`]s.
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

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::error::CassError;
use super::process::CassInvocation;

/// Default binary name `ee` resolves through `$PATH` when the config
/// does not pin an explicit location.
pub const DEFAULT_BINARY: &str = "cass";

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
}

impl CassClient {
    /// Build a client that resolves the default `cass` binary through
    /// `$PATH`.
    #[must_use]
    pub fn new_default() -> Self {
        Self::with_binary(DEFAULT_BINARY)
    }

    /// Build a client that records `binary` in the invocation intent.
    ///
    /// EE-100 only spawns the fixed `cass` executable; explicit binary
    /// path discovery and validation land in EE-101. Until then,
    /// non-default binaries are useful for provenance tests but
    /// [`CassInvocation::run`] rejects them with `invalid_binary`.
    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            extra_env: Vec::new(),
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

    /// Build a single [`CassInvocation`] for `cass <args...>`. The
    /// stable env overrides are always applied; per-call user env adds
    /// to them.
    pub fn invocation<I, S>(&self, args: I) -> CassInvocation
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut inv = CassInvocation::new(self.binary.clone(), args);
        for (key, value) in STABLE_ENV_OVERRIDES {
            inv = inv.with_env(*key, *value);
        }
        for (key, value) in &self.extra_env {
            inv = inv.with_env(key.clone(), value.clone());
        }
        inv
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
            "--request-id".to_owned(),
            request_id.to_owned(),
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
    use super::{CassClient, DEFAULT_BINARY, STABLE_ENV_OVERRIDES};

    use std::path::Path;

    type TestResult = Result<(), String>;

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
    fn run_rejects_non_default_binary_before_spawn() -> TestResult {
        let client = CassClient::with_binary("/no/such/cass-binary-eeplaceholder");
        let inv = client.invocation(["health", "--json"]);
        let error = match client.run(&inv) {
            Ok(_) => return Err("custom binary should fail before spawn".to_string()),
            Err(error) => error,
        };
        assert_eq!(error.kind_str(), "invalid_binary");
        Ok(())
    }
}
