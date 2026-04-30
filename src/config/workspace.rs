//! Workspace detection and canonicalization (EE-023, EE-PRIV-WS-001).
//!
//! `ee` resolves the active workspace by walking *upward* from a
//! starting directory looking for a `.ee/` subdirectory. The walk
//! stops at the first match, so a nested project with its own `.ee/`
//! takes precedence over a containing one — the same convention git,
//! cargo, and most modern CLIs use.
//!
//! The discovery routine performs no I/O beyond `is_dir` checks, so
//! it is both cheap and offline-friendly. Symlinks are followed
//! naturally because `Path::is_dir` follows them; if a developer
//! intentionally points `~/work/foo/.ee` at a sibling repo's `.ee`,
//! discovery treats the symlinked target as the workspace.
//!
//! `discover` is purely lexical: it does not consult the database,
//! registered aliases, or environment overrides. `resolve_workspace`
//! layers deterministic explicit-path and environment precedence on
//! top of that discovery result without creating directories.
//!
//! ## Workspace Canonicalization (EE-PRIV-WS-001)
//!
//! All workspace paths are canonicalized to prevent identity confusion:
//! - Symlinks in the path chain are resolved to detect potential leakage
//! - A per-installation salt ensures the same path produces different
//!   hashes on different machines (privacy protection)
//! - By default, workspaces are rejected if the input path differs from
//!   the canonical path due to symlinks (prevents cross-workspace leaks)

use std::env;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::PathExpander;

/// Default subdirectory marker for an `ee` workspace.
///
/// `<workspace>/.ee/` is the project-local directory called out in
/// the README's storage layout.
pub const WORKSPACE_MARKER: &str = ".ee";

/// Environment variable used as a process-wide workspace override.
pub const WORKSPACE_ENV_VAR: &str = "EE_WORKSPACE";

/// Schema identifier for workspace identity records.
pub const WORKSPACE_IDENTITY_SCHEMA_V1: &str = "ee.workspace.identity.v1";

/// How platform case sensitivity is handled for workspace identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformCaseHandling {
    /// Preserve original case (case-sensitive filesystems).
    Preserve,
    /// Convert to lowercase for hashing (case-insensitive filesystems).
    Lower,
}

impl PlatformCaseHandling {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preserve => "preserve",
            Self::Lower => "lower",
        }
    }

    #[must_use]
    pub fn current_platform() -> Self {
        if cfg!(target_os = "windows") || cfg!(target_os = "macos") {
            Self::Lower
        } else {
            Self::Preserve
        }
    }
}

/// A symlink traversal record in the path chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SymlinkTraversal {
    /// The path component that was a symlink.
    pub link_path: PathBuf,
    /// The target that the symlink points to.
    pub target_path: PathBuf,
}

/// Result of canonicalizing a workspace path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CanonicalWorkspace {
    /// The original input path before canonicalization.
    pub input_path: PathBuf,
    /// The fully resolved canonical path.
    pub canonical_path: PathBuf,
    /// Salted hash used as workspace identity (BLAKE3, 24 hex chars).
    pub salted_hash: String,
    /// Symlinks traversed during canonicalization.
    pub symlink_chain: Vec<SymlinkTraversal>,
    /// How case is handled on this platform.
    pub platform_case_handling: PlatformCaseHandling,
    /// Git repository root, if this is inside a git repo.
    pub git_root: Option<PathBuf>,
    /// Git worktree name, if this is a worktree.
    pub worktree: Option<String>,
    /// Whether this appears to be a fork (origin remote differs).
    pub fork: Option<bool>,
}

impl CanonicalWorkspace {
    /// Check if the path involved any symlink traversal.
    #[must_use]
    pub fn has_symlinks(&self) -> bool {
        !self.symlink_chain.is_empty()
    }

    /// Check if the input path differs from canonical due to symlinks.
    #[must_use]
    pub fn input_differs_from_canonical(&self) -> bool {
        self.input_path != self.canonical_path
    }
}

/// Policy for handling symlinks in workspace paths.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SymlinkPolicy {
    /// Reject workspaces where input path differs from canonical due to symlinks.
    #[default]
    Deny,
    /// Allow symlinks (user opted in with --allow-symlink).
    Allow,
}

/// Error returned when workspace canonicalization fails.
#[derive(Debug)]
pub enum CanonicalizationError {
    /// The path could not be canonicalized (doesn't exist, permissions, etc.).
    CanonicalizeFailure { path: PathBuf, source: io::Error },
    /// A symlink was detected and the policy denies it.
    SymlinkBlocked {
        input_path: PathBuf,
        canonical_path: PathBuf,
        symlink_chain: Vec<SymlinkTraversal>,
    },
    /// Failed to read the installation salt.
    SaltReadFailure { path: PathBuf, source: io::Error },
    /// Failed to create the installation salt.
    SaltCreateFailure { path: PathBuf, source: io::Error },
}

impl std::fmt::Display for CanonicalizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CanonicalizeFailure { path, source } => {
                write!(
                    f,
                    "failed to canonicalize path {}: {}",
                    path.display(),
                    source
                )
            }
            Self::SymlinkBlocked {
                input_path,
                canonical_path,
                symlink_chain,
            } => {
                write!(
                    f,
                    "workspace path {} resolves to {} through {} symlink(s); \
                     symlinks are blocked by default policy",
                    input_path.display(),
                    canonical_path.display(),
                    symlink_chain.len()
                )
            }
            Self::SaltReadFailure { path, source } => {
                write!(
                    f,
                    "failed to read installation salt from {}: {}",
                    path.display(),
                    source
                )
            }
            Self::SaltCreateFailure { path, source } => {
                write!(
                    f,
                    "failed to create installation salt at {}: {}",
                    path.display(),
                    source
                )
            }
        }
    }
}

impl std::error::Error for CanonicalizationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CanonicalizeFailure { source, .. }
            | Self::SaltReadFailure { source, .. }
            | Self::SaltCreateFailure { source, .. } => Some(source),
            Self::SymlinkBlocked { .. } => None,
        }
    }
}

impl CanonicalizationError {
    /// Return the error code for JSON output.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CanonicalizeFailure { .. } => "workspace_canonicalize_failed",
            Self::SymlinkBlocked { .. } => "workspace_symlink_blocked",
            Self::SaltReadFailure { .. } => "workspace_salt_read_failed",
            Self::SaltCreateFailure { .. } => "workspace_salt_create_failed",
        }
    }

    /// Return a repair suggestion for this error.
    #[must_use]
    pub fn repair(&self) -> String {
        match self {
            Self::CanonicalizeFailure { path, .. } => {
                format!("Verify that {} exists and is accessible.", path.display())
            }
            Self::SymlinkBlocked { input_path, .. } => {
                format!(
                    "Use `ee init --allow-symlink {}` to permit symlinks, \
                     or use the canonical path directly.",
                    input_path.display()
                )
            }
            Self::SaltReadFailure { path, .. } | Self::SaltCreateFailure { path, .. } => {
                format!(
                    "Check permissions on {} and run `ee doctor --fix-plan`.",
                    path.display()
                )
            }
        }
    }
}

/// Canonicalize a workspace path with symlink detection and salted hashing.
///
/// This function:
/// 1. Resolves the input path to its canonical form
/// 2. Detects any symlinks in the path chain
/// 3. Generates a salted hash for workspace identity
/// 4. Optionally detects git repository and worktree info
///
/// # Arguments
///
/// * `input_path` - The path to canonicalize
/// * `salt` - The per-installation salt for hashing
/// * `policy` - How to handle symlinks in the path
///
/// # Errors
///
/// Returns [`CanonicalizationError::CanonicalizeFailure`] if the path cannot be resolved.
/// Returns [`CanonicalizationError::SymlinkBlocked`] if symlinks are detected and policy is Deny.
pub fn canonicalize_workspace_path(
    input_path: &Path,
    salt: &[u8],
    policy: SymlinkPolicy,
) -> Result<CanonicalWorkspace, CanonicalizationError> {
    let canonical_path =
        input_path
            .canonicalize()
            .map_err(|source| CanonicalizationError::CanonicalizeFailure {
                path: input_path.to_path_buf(),
                source,
            })?;

    let symlink_chain = detect_symlinks(input_path, &canonical_path);

    if policy == SymlinkPolicy::Deny && !symlink_chain.is_empty() && input_path != canonical_path {
        return Err(CanonicalizationError::SymlinkBlocked {
            input_path: input_path.to_path_buf(),
            canonical_path,
            symlink_chain,
        });
    }

    let platform_case_handling = PlatformCaseHandling::current_platform();
    let salted_hash = compute_salted_workspace_hash(&canonical_path, salt, platform_case_handling);

    let git_root = detect_git_root(&canonical_path);
    let worktree = git_root.as_ref().and_then(|root| detect_git_worktree(root));
    let fork = None; // Future: detect by comparing origin remote

    Ok(CanonicalWorkspace {
        input_path: input_path.to_path_buf(),
        canonical_path,
        salted_hash,
        symlink_chain,
        platform_case_handling,
        git_root,
        worktree,
        fork,
    })
}

/// Detect symlinks by comparing input path components to canonical path.
fn detect_symlinks(input_path: &Path, canonical_path: &Path) -> Vec<SymlinkTraversal> {
    let mut symlinks = Vec::new();
    let mut current = PathBuf::new();

    for component in input_path.components() {
        match component {
            Component::Prefix(p) => current.push(p.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                current.pop();
            }
            Component::Normal(segment) => {
                current.push(segment);
                if current.is_symlink() {
                    if let Ok(target) = fs::read_link(&current) {
                        symlinks.push(SymlinkTraversal {
                            link_path: current.clone(),
                            target_path: target,
                        });
                    }
                }
            }
        }
    }

    // Also check if the final paths differ even without explicit symlink detection
    if symlinks.is_empty() && input_path != canonical_path {
        // The input and canonical differ but we didn't find individual symlinks.
        // This can happen with bind mounts or other filesystem features.
        // We don't add a fake symlink entry here - the paths just differ.
    }

    symlinks
}

/// Compute a salted BLAKE3 hash of the workspace path.
fn compute_salted_workspace_hash(
    canonical_path: &Path,
    salt: &[u8],
    case_handling: PlatformCaseHandling,
) -> String {
    let path_str = canonical_path.to_string_lossy();
    let normalized = match case_handling {
        PlatformCaseHandling::Preserve => path_str.to_string(),
        PlatformCaseHandling::Lower => path_str.to_lowercase(),
    };

    let mut hasher = blake3::Hasher::new_keyed(&derive_key_from_salt(salt));
    hasher.update(normalized.as_bytes());
    let hash = hasher.finalize().to_hex();
    hash.chars().take(24).collect()
}

/// Derive a 32-byte key from the installation salt.
fn derive_key_from_salt(salt: &[u8]) -> [u8; 32] {
    let hash = blake3::hash(salt);
    *hash.as_bytes()
}

/// Detect the git repository root containing this path.
fn detect_git_root(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    loop {
        let git_dir = current.join(".git");
        if git_dir.exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
}

/// Detect if the git repository is a worktree.
fn detect_git_worktree(git_root: &Path) -> Option<String> {
    let git_file = git_root.join(".git");
    if git_file.is_file() {
        // This is a worktree - .git is a file pointing to the main repo
        if let Ok(content) = fs::read_to_string(&git_file) {
            if content.starts_with("gitdir:") {
                // Extract worktree name from path like .git/worktrees/<name>
                if let Some(worktree_path) = content.strip_prefix("gitdir:") {
                    let trimmed = worktree_path.trim();
                    if let Some(idx) = trimmed.rfind("/worktrees/") {
                        let name = &trimmed[idx + 11..];
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Get or create the per-installation salt.
///
/// The salt is stored at `~/.local/share/ee/.salt` with mode 0600.
/// It is created on first run and never rotated automatically.
///
/// # Errors
///
/// Returns an error if the salt cannot be read or created.
pub fn get_or_create_installation_salt() -> Result<Vec<u8>, CanonicalizationError> {
    let salt_path = get_salt_path();

    if salt_path.exists() {
        fs::read(&salt_path).map_err(|source| CanonicalizationError::SaltReadFailure {
            path: salt_path,
            source,
        })
    } else {
        create_installation_salt(&salt_path)
    }
}

fn get_salt_path() -> PathBuf {
    // XDG_DATA_HOME or ~/.local/share/ee/.salt
    if let Ok(xdg_data) = env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg_data).join("ee").join(".salt");
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".local/share/ee/.salt");
    }
    // Fallback for unusual environments
    PathBuf::from("/tmp/ee/.salt")
}

fn create_installation_salt(salt_path: &Path) -> Result<Vec<u8>, CanonicalizationError> {
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = salt_path.parent() {
        fs::create_dir_all(parent).map_err(|source| CanonicalizationError::SaltCreateFailure {
            path: salt_path.to_path_buf(),
            source,
        })?;
    }

    // Generate 32 bytes of random salt
    let salt: [u8; 32] = rand_salt();

    // Write with mode 0600 (owner read/write only)
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);

    let mut file =
        options
            .open(salt_path)
            .map_err(|source| CanonicalizationError::SaltCreateFailure {
                path: salt_path.to_path_buf(),
                source,
            })?;

    io::Write::write_all(&mut file, &salt).map_err(|source| {
        CanonicalizationError::SaltCreateFailure {
            path: salt_path.to_path_buf(),
            source,
        }
    })?;

    Ok(salt.to_vec())
}

fn rand_salt() -> [u8; 32] {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Simple entropy source: combine process ID, time, and a counter
    let pid = std::process::id();
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let mut seed = [0u8; 32];
    let pid_bytes = pid.to_le_bytes();
    let time_bytes = time.to_le_bytes();

    seed[..4].copy_from_slice(&pid_bytes);
    seed[4..20].copy_from_slice(&time_bytes);

    // Hash to spread entropy
    *blake3::hash(&seed).as_bytes()
}

/// A successfully-discovered workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceLocation {
    /// Directory that *contains* the `.ee/` marker. The project root.
    pub root: PathBuf,
    /// The full path to the `.ee/` directory itself.
    pub config_dir: PathBuf,
}

impl WorkspaceLocation {
    /// Construct a [`WorkspaceLocation`] from an explicit root,
    /// computing the config directory as `<root>/.ee`.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        let config_dir = root.join(WORKSPACE_MARKER);
        Self { root, config_dir }
    }
}

/// Which input selected the active workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceResolutionSource {
    /// A CLI flag or direct API argument supplied the workspace root.
    Explicit,
    /// [`WORKSPACE_ENV_VAR`] supplied the workspace root.
    Environment,
    /// The resolver found the nearest ancestor containing `.ee/`.
    Discovered,
    /// No marker was required, so the current directory became the root.
    CurrentDirectory,
}

impl WorkspaceResolutionSource {
    /// Stable machine-facing spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Environment => "environment",
            Self::Discovered => "discovered",
            Self::CurrentDirectory => "current_directory",
        }
    }
}

/// Whether resolution must find an initialized workspace marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceResolutionMode {
    /// Require `<workspace>/.ee/` to exist.
    ExistingOnly,
    /// Allow a root without `.ee/`; used by initializing commands.
    AllowUninitialized,
}

/// Inputs for deterministic workspace resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceResolutionRequest {
    /// Direct workspace path from a CLI flag or caller.
    pub explicit_workspace: Option<PathBuf>,
    /// Workspace path from [`WORKSPACE_ENV_VAR`].
    pub environment_workspace: Option<PathBuf>,
    /// Directory used for relative paths and upward discovery.
    pub current_dir: PathBuf,
    /// Marker requirement policy.
    pub mode: WorkspaceResolutionMode,
}

impl WorkspaceResolutionRequest {
    /// Build a request with explicit dependencies for deterministic tests.
    #[must_use]
    pub fn new(current_dir: PathBuf, mode: WorkspaceResolutionMode) -> Self {
        Self {
            explicit_workspace: None,
            environment_workspace: None,
            current_dir,
            mode,
        }
    }

    /// Attach a direct workspace path.
    #[must_use]
    pub fn with_explicit_workspace(mut self, workspace: PathBuf) -> Self {
        self.explicit_workspace = Some(workspace);
        self
    }

    /// Attach an environment workspace path.
    #[must_use]
    pub fn with_environment_workspace(mut self, workspace: PathBuf) -> Self {
        self.environment_workspace = Some(workspace);
        self
    }

    /// Build from the current process environment.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError::CurrentDir`] if the process cwd cannot be read.
    pub fn from_process(
        explicit_workspace: Option<PathBuf>,
        mode: WorkspaceResolutionMode,
    ) -> Result<Self, WorkspaceError> {
        let current_dir = env::current_dir().map_err(WorkspaceError::CurrentDir)?;
        Ok(Self {
            explicit_workspace,
            environment_workspace: env::var_os(WORKSPACE_ENV_VAR).map(PathBuf::from),
            current_dir,
            mode,
        })
    }
}

/// Result of resolving the active workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceResolution {
    /// Resolved workspace location.
    pub location: WorkspaceLocation,
    /// Source that won precedence.
    pub source: WorkspaceResolutionSource,
    /// Whether `<root>/.ee/` existed at resolution time.
    pub marker_present: bool,
    /// Canonical root when filesystem canonicalization succeeds; otherwise
    /// a lexical absolute root.
    pub canonical_root: PathBuf,
    /// Stable local identity fingerprint derived from `canonical_root`.
    pub fingerprint: String,
}

impl WorkspaceResolution {
    fn new(location: WorkspaceLocation, source: WorkspaceResolutionSource) -> Self {
        let marker_present = location.config_dir.is_dir();
        let canonical_root = canonical_or_lexical(&location.root);
        let fingerprint = workspace_fingerprint(&canonical_root);
        Self {
            location,
            source,
            marker_present,
            canonical_root,
            fingerprint,
        }
    }
}

/// Errors returned by [`discover_from_current_dir`].
///
/// [`discover`] never returns an error — missing paths simply yield
/// `Ok(None)` because workspace detection is informational, not a
/// gate. [`discover_from_current_dir`] surfaces the underlying I/O
/// error from [`std::env::current_dir`] when the cwd cannot be read.
#[derive(Debug)]
pub enum WorkspaceError {
    /// `std::env::current_dir` failed (process has no cwd, etc.).
    CurrentDir(io::Error),
    /// Resolution required an initialized workspace but none was found.
    NotFound { start: PathBuf },
    /// An explicit or environment root was chosen but lacks `.ee/`.
    MissingMarker {
        source: WorkspaceResolutionSource,
        root: PathBuf,
    },
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CurrentDir(error) => write!(
                formatter,
                "failed to read the current working directory: {error}"
            ),
            Self::NotFound { start } => {
                write!(formatter, "no ee workspace found from {}", start.display())
            }
            Self::MissingMarker { source, root } => write!(
                formatter,
                "{} workspace {} is not initialized; expected {}/{}",
                source.as_str(),
                root.display(),
                root.display(),
                WORKSPACE_MARKER
            ),
        }
    }
}

impl std::error::Error for WorkspaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CurrentDir(error) => Some(error),
            Self::NotFound { .. } | Self::MissingMarker { .. } => None,
        }
    }
}

/// Resolve the active workspace using stable precedence:
/// explicit path, [`WORKSPACE_ENV_VAR`], nearest `.ee/` ancestor, then
/// current directory when uninitialized workspaces are allowed.
///
/// Paths are expanded with [`PathExpander`] and made absolute relative
/// to [`WorkspaceResolutionRequest::current_dir`]. No directory is
/// created and no durable state is mutated.
///
/// # Errors
///
/// Returns [`WorkspaceError::MissingMarker`] when a selected explicit or
/// environment root lacks `.ee/` in [`WorkspaceResolutionMode::ExistingOnly`].
/// Returns [`WorkspaceError::NotFound`] when no marker can be discovered.
pub fn resolve_workspace(
    request: &WorkspaceResolutionRequest,
) -> Result<WorkspaceResolution, WorkspaceError> {
    if let Some(path) = request.explicit_workspace.as_ref() {
        return resolve_selected_root(request, path, WorkspaceResolutionSource::Explicit);
    }
    if let Some(path) = request.environment_workspace.as_ref() {
        return resolve_selected_root(request, path, WorkspaceResolutionSource::Environment);
    }
    if let Some(location) = discover(&request.current_dir) {
        return Ok(WorkspaceResolution::new(
            absolutize_location(&request.current_dir, location),
            WorkspaceResolutionSource::Discovered,
        ));
    }
    if request.mode == WorkspaceResolutionMode::AllowUninitialized {
        let root = lexical_absolute(&request.current_dir, Path::new("."));
        return Ok(WorkspaceResolution::new(
            WorkspaceLocation::new(root),
            WorkspaceResolutionSource::CurrentDirectory,
        ));
    }
    Err(WorkspaceError::NotFound {
        start: request.current_dir.clone(),
    })
}

fn resolve_selected_root(
    request: &WorkspaceResolutionRequest,
    raw: &Path,
    source: WorkspaceResolutionSource,
) -> Result<WorkspaceResolution, WorkspaceError> {
    let expanded = expand_selected_path(raw);
    let root = lexical_absolute(&request.current_dir, &expanded);
    let location = WorkspaceLocation::new(root);
    if request.mode == WorkspaceResolutionMode::ExistingOnly && !location.config_dir.is_dir() {
        return Err(WorkspaceError::MissingMarker {
            source,
            root: location.root,
        });
    }
    Ok(WorkspaceResolution::new(location, source))
}

fn expand_selected_path(raw: &Path) -> PathBuf {
    let Some(raw_str) = raw.to_str() else {
        return raw.to_path_buf();
    };
    match PathExpander::from_process_env().expand(raw_str) {
        Ok(path) => path,
        Err(_) => raw.to_path_buf(),
    }
}

fn absolutize_location(base: &Path, location: WorkspaceLocation) -> WorkspaceLocation {
    WorkspaceLocation::new(lexical_absolute(base, &location.root))
}

fn canonical_or_lexical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| lexical_absolute(Path::new("."), path))
}

fn workspace_fingerprint(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    let hash = blake3::hash(rendered.as_bytes()).to_hex();
    hash.chars().take(24).collect()
}

fn lexical_absolute(base: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    normalize_lexical(&joined)
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !path.is_absolute() {
                    out.push("..");
                }
            }
            Component::Normal(segment) => out.push(segment),
        }
    }
    out
}

/// Walk upward from `start` looking for the closest ancestor whose
/// children include a `.ee/` directory.
///
/// Returns `Ok(Some(location))` when a workspace is found. Returns
/// `Ok(None)` when the walk reaches the filesystem root without
/// finding `.ee`. Never panics; missing or unreadable paths are
/// treated as "no workspace here" and the walk continues upward.
///
/// `start` may be either an absolute or relative path. The function
/// does not canonicalize: a relative path stays relative in the
/// returned [`WorkspaceLocation::root`]. Callers that need
/// canonicalization should call [`Path::canonicalize`] before passing
/// the path in.
#[must_use]
pub fn discover(start: &Path) -> Option<WorkspaceLocation> {
    let mut current = start;
    loop {
        let candidate = current.join(WORKSPACE_MARKER);
        if candidate.is_dir() {
            return Some(WorkspaceLocation {
                root: current.to_path_buf(),
                config_dir: candidate,
            });
        }
        let parent = current.parent()?;
        if parent == current {
            // `Path::parent` returns `Some(self)` for the
            // empty path, which would otherwise loop forever.
            return None;
        }
        current = parent;
    }
}

/// Like [`discover`], but starts from [`std::env::current_dir`].
///
/// # Errors
///
/// Returns [`WorkspaceError::CurrentDir`] when the current working
/// directory cannot be read (process has no cwd, permissions denied,
/// etc.). A successful read followed by no match still returns
/// `Ok(None)`.
pub fn discover_from_current_dir() -> Result<Option<WorkspaceLocation>, WorkspaceError> {
    let cwd = env::current_dir().map_err(WorkspaceError::CurrentDir)?;
    Ok(discover(&cwd))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    use uuid::Uuid;

    use super::{
        WORKSPACE_ENV_VAR, WORKSPACE_MARKER, WorkspaceError, WorkspaceLocation,
        WorkspaceResolutionMode, WorkspaceResolutionRequest, WorkspaceResolutionSource, discover,
        discover_from_current_dir, resolve_workspace,
    };

    type TestResult = Result<(), String>;

    /// Counter so two tests within the same process never share a
    /// scratch directory even if `Uuid::now_v7` collides at the
    /// millisecond boundary.
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Tiny scratch-directory helper to keep tests hermetic without
    /// adding a `tempfile` direct dependency.
    struct ScratchDir {
        root: PathBuf,
    }

    impl ScratchDir {
        fn new(label: &str) -> Result<Self, String> {
            let id = COUNTER.fetch_add(1, Ordering::SeqCst);
            let suffix = format!("ee-ws-{label}-{}-{id}", Uuid::now_v7().simple());
            let root = std::env::temp_dir().join(suffix);
            if let Err(error) = fs::create_dir_all(&root) {
                return Err(format!("failed to create scratch dir at {root:?}: {error}"));
            }
            Ok(Self { root })
        }

        fn path(&self) -> &Path {
            &self.root
        }

        fn make_dir(&self, relative: &str) -> Result<PathBuf, String> {
            let path = self.root.join(relative);
            if let Err(error) = fs::create_dir_all(&path) {
                return Err(format!("failed to create {path:?}: {error}"));
            }
            Ok(path)
        }

        fn make_file(&self, relative: &str, contents: &str) -> Result<PathBuf, String> {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                if let Err(error) = fs::create_dir_all(parent) {
                    return Err(format!("failed to create parent of {path:?}: {error}"));
                }
            }
            let mut file = match fs::File::create(&path) {
                Ok(value) => value,
                Err(error) => return Err(format!("failed to create {path:?}: {error}")),
            };
            if let Err(error) = file.write_all(contents.as_bytes()) {
                return Err(format!("failed to write {path:?}: {error}"));
            }
            Ok(path)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            // Best-effort cleanup; failures here are not test failures.
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn discover_finds_marker_directly_above() -> TestResult {
        let scratch = ScratchDir::new("direct")?;
        let workspace = scratch.make_dir("project")?;
        let _marker = scratch.make_dir("project/.ee")?;

        let location = match discover(&workspace) {
            Some(value) => value,
            None => return Err(format!("expected to find workspace at {workspace:?}")),
        };
        assert_eq!(location.root, workspace);
        assert_eq!(location.config_dir, workspace.join(".ee"));
        Ok(())
    }

    #[test]
    fn discover_walks_up_through_nested_directories() -> TestResult {
        let scratch = ScratchDir::new("nested")?;
        let project = scratch.make_dir("project")?;
        scratch.make_dir("project/.ee")?;
        let nested = scratch.make_dir("project/src/deep/leaf")?;

        let location = match discover(&nested) {
            Some(value) => value,
            None => return Err("expected to find workspace by walking up".to_string()),
        };
        assert_eq!(location.root, project);
        assert_eq!(location.config_dir, project.join(".ee"));
        Ok(())
    }

    #[test]
    fn discover_picks_closest_marker_when_nested_workspaces_exist() -> TestResult {
        let scratch = ScratchDir::new("nested-ws")?;
        let _outer = scratch.make_dir("outer")?;
        scratch.make_dir("outer/.ee")?;
        let inner = scratch.make_dir("outer/inner")?;
        scratch.make_dir("outer/inner/.ee")?;
        let leaf = scratch.make_dir("outer/inner/sub")?;

        let location = match discover(&leaf) {
            Some(value) => value,
            None => return Err("expected nested workspace match".to_string()),
        };
        assert_eq!(location.root, inner);
        Ok(())
    }

    #[test]
    fn discover_returns_none_when_no_marker_exists() -> TestResult {
        let scratch = ScratchDir::new("none")?;
        let leaf = scratch.make_dir("a/b/c")?;
        let result = discover(&leaf);
        // The scratch dir lives inside `std::env::temp_dir()`, which
        // typically does not have a `.ee` ancestor — but we guard
        // against the (extremely unlikely) case that it does by
        // requiring discovery to terminate at a sane boundary.
        match result {
            None => {}
            Some(location) => {
                // If a discovery happened, it must be outside the
                // scratch tree. That still proves discovery walked
                // upward, but we can at least assert it did not
                // hallucinate a marker inside the scratch dir.
                assert!(
                    !location.root.starts_with(scratch.path()),
                    "unexpected discovery inside scratch dir at {:?}",
                    location.root
                );
            }
        }
        Ok(())
    }

    #[test]
    fn discover_ignores_marker_when_it_is_a_file() -> TestResult {
        let scratch = ScratchDir::new("marker-file")?;
        let dir = scratch.make_dir("project")?;
        let _file = scratch.make_file("project/.ee", "this is a file, not a dir")?;
        // Walk above the project, since discover doesn't accept the
        // file-as-marker. The result should not be the project dir.
        let result = discover(&dir);
        if let Some(location) = result {
            assert_ne!(
                location.root, dir,
                "discover treated a file named .ee as a workspace"
            );
        }
        Ok(())
    }

    #[test]
    fn discover_handles_root_path_without_panicking() {
        // Walking up from "/" must terminate. Most filesystems do not
        // have a `.ee` at the root; if the test host is unusual the
        // assertion still holds because we only require termination
        // and a deterministic Some/None.
        let result = discover(Path::new("/"));
        // Either Some(/) (if /.ee exists) or None — both are valid.
        if let Some(location) = result {
            assert_eq!(location.root, Path::new("/"));
        }
    }

    #[test]
    fn discover_handles_empty_path_without_panicking() {
        // `Path::parent` on the empty path yields `Some("")` which
        // would otherwise loop forever; the implementation guards
        // against this with a `parent == current` check.
        let result = discover(Path::new(""));
        // Either resolves through the cwd or is None; the contract is
        // termination + no panic.
        let _ = result;
    }

    #[test]
    fn discover_does_not_canonicalise_input_path() -> TestResult {
        let scratch = ScratchDir::new("canon")?;
        scratch.make_dir("project/.ee")?;
        let leaf = scratch.make_dir("project/src")?;
        // Build a relative-ish path with `.` segments. `discover`
        // should preserve the lexical shape rather than canonicalise.
        let with_dots = leaf.join(".").join(".");
        let location = match discover(&with_dots) {
            Some(value) => value,
            None => return Err("expected discovery".to_string()),
        };
        // The reported root contains the upward walk's lexical form.
        assert!(location.root.ends_with("project"));
        Ok(())
    }

    #[test]
    fn resolve_prefers_explicit_workspace_over_env_and_discovery() -> TestResult {
        let scratch = ScratchDir::new("resolve-explicit")?;
        let cwd = scratch.make_dir("current")?;
        scratch.make_dir("current/.ee")?;
        let env_workspace = scratch.make_dir("env")?;
        scratch.make_dir("env/.ee")?;
        let explicit = scratch.path().join("explicit");

        let request =
            WorkspaceResolutionRequest::new(cwd, WorkspaceResolutionMode::AllowUninitialized)
                .with_environment_workspace(env_workspace)
                .with_explicit_workspace(explicit.clone());

        let resolved = resolve_workspace(&request).map_err(|error| error.to_string())?;
        assert_eq!(resolved.source, WorkspaceResolutionSource::Explicit);
        assert_eq!(resolved.location.root, explicit);
        assert!(!resolved.marker_present);
        assert_eq!(
            resolved.location.config_dir,
            explicit.join(WORKSPACE_MARKER)
        );
        Ok(())
    }

    #[test]
    fn resolve_rejects_selected_root_without_marker_when_existing_required() -> TestResult {
        let scratch = ScratchDir::new("resolve-missing-marker")?;
        let cwd = scratch.make_dir("current")?;
        let explicit = scratch.make_dir("explicit")?;
        let request = WorkspaceResolutionRequest::new(cwd, WorkspaceResolutionMode::ExistingOnly)
            .with_explicit_workspace(explicit.clone());

        match resolve_workspace(&request) {
            Err(WorkspaceError::MissingMarker { source, root }) => {
                assert_eq!(source, WorkspaceResolutionSource::Explicit);
                assert_eq!(root, explicit);
                Ok(())
            }
            other => Err(format!("expected MissingMarker, got {other:?}")),
        }
    }

    #[test]
    fn resolve_uses_environment_workspace_when_explicit_is_absent() -> TestResult {
        let scratch = ScratchDir::new("resolve-env")?;
        let cwd = scratch.make_dir("current")?;
        let env_workspace = scratch.make_dir("env")?;
        scratch.make_dir("env/.ee")?;
        let request = WorkspaceResolutionRequest::new(cwd, WorkspaceResolutionMode::ExistingOnly)
            .with_environment_workspace(env_workspace.clone());

        let resolved = resolve_workspace(&request).map_err(|error| error.to_string())?;
        assert_eq!(resolved.source, WorkspaceResolutionSource::Environment);
        assert_eq!(resolved.location.root, env_workspace);
        assert!(resolved.marker_present);
        assert_eq!(
            WorkspaceResolutionSource::Environment.as_str(),
            "environment"
        );
        assert_eq!(WORKSPACE_ENV_VAR, "EE_WORKSPACE");
        Ok(())
    }

    #[test]
    fn resolve_discovers_nearest_workspace_and_sets_stable_fingerprint() -> TestResult {
        let scratch = ScratchDir::new("resolve-discover")?;
        let project = scratch.make_dir("project")?;
        scratch.make_dir("project/.ee")?;
        let leaf = scratch.make_dir("project/src/leaf")?;
        let request = WorkspaceResolutionRequest::new(leaf, WorkspaceResolutionMode::ExistingOnly);

        let first = resolve_workspace(&request).map_err(|error| error.to_string())?;
        let second = resolve_workspace(&request).map_err(|error| error.to_string())?;

        assert_eq!(first.source, WorkspaceResolutionSource::Discovered);
        assert_eq!(first.location.root, project);
        assert!(first.marker_present);
        assert_eq!(first.fingerprint.len(), 24);
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.canonical_root, second.canonical_root);
        Ok(())
    }

    #[test]
    fn resolve_allows_current_directory_for_uninitialized_workspace() -> TestResult {
        let scratch = ScratchDir::new("resolve-uninit")?;
        let cwd = scratch.make_dir("new-project")?;
        let request = WorkspaceResolutionRequest::new(
            cwd.clone(),
            WorkspaceResolutionMode::AllowUninitialized,
        );

        let resolved = resolve_workspace(&request).map_err(|error| error.to_string())?;
        assert_eq!(resolved.source, WorkspaceResolutionSource::CurrentDirectory);
        assert_eq!(resolved.location.root, cwd);
        assert!(!resolved.marker_present);
        Ok(())
    }

    #[test]
    fn resolve_errors_when_no_workspace_exists_and_marker_is_required() -> TestResult {
        let scratch = ScratchDir::new("resolve-not-found")?;
        let cwd = scratch.make_dir("no-marker")?;
        let request =
            WorkspaceResolutionRequest::new(cwd.clone(), WorkspaceResolutionMode::ExistingOnly);

        match resolve_workspace(&request) {
            Err(WorkspaceError::NotFound { start }) => {
                assert_eq!(start, cwd);
                Ok(())
            }
            other => Err(format!("expected NotFound, got {other:?}")),
        }
    }

    #[test]
    fn resolve_anchors_relative_selected_paths_to_current_dir() -> TestResult {
        let scratch = ScratchDir::new("resolve-relative")?;
        let cwd = scratch.make_dir("parent/current")?;
        let workspace = scratch.make_dir("parent/workspace")?;
        scratch.make_dir("parent/workspace/.ee")?;
        let request = WorkspaceResolutionRequest::new(cwd, WorkspaceResolutionMode::ExistingOnly)
            .with_explicit_workspace(PathBuf::from("../workspace"));

        let resolved = resolve_workspace(&request).map_err(|error| error.to_string())?;
        assert_eq!(resolved.location.root, workspace);
        assert!(resolved.marker_present);
        Ok(())
    }

    #[test]
    fn workspace_location_new_computes_config_dir() {
        let location = WorkspaceLocation::new(PathBuf::from("/tmp/example"));
        assert_eq!(location.root, PathBuf::from("/tmp/example"));
        assert_eq!(location.config_dir, PathBuf::from("/tmp/example/.ee"));
    }

    #[test]
    fn discover_from_current_dir_succeeds_or_returns_none() -> TestResult {
        // Cannot mutate cwd here without affecting other tests, so
        // this test just exercises the API surface: the call must
        // either succeed (with Some/None) or return a structured
        // error.
        match discover_from_current_dir() {
            Ok(_) => {}
            Err(WorkspaceError::CurrentDir(error)) => {
                let rendered = error.to_string();
                assert!(!rendered.is_empty());
            }
            Err(error) => return Err(format!("unexpected workspace error: {error}")),
        }
        Ok(())
    }

    // --- Canonicalization tests (EE-PRIV-WS-001) ---

    use super::{
        CanonicalizationError, PlatformCaseHandling, SymlinkPolicy, canonicalize_workspace_path,
    };

    #[test]
    fn canonicalize_simple_path_succeeds() -> TestResult {
        let scratch = ScratchDir::new("canon-simple")?;
        let project = scratch.make_dir("project")?;
        let salt = b"test-salt-12345678901234567890123";

        let result = canonicalize_workspace_path(&project, salt, SymlinkPolicy::Allow)
            .map_err(|e| e.to_string())?;

        assert_eq!(result.input_path, project);
        assert_eq!(result.canonical_path, project.canonicalize().unwrap());
        assert!(!result.has_symlinks());
        assert_eq!(result.salted_hash.len(), 24);
        Ok(())
    }

    #[test]
    fn canonicalize_path_through_symlink_detects_it() -> TestResult {
        let scratch = ScratchDir::new("canon-symlink")?;
        let target = scratch.make_dir("real-project")?;
        let link = scratch.path().join("linked-project");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link)
            .map_err(|e| format!("symlink creation failed: {e}"))?;

        #[cfg(not(unix))]
        return Ok(()); // Skip on non-Unix

        let salt = b"test-salt-12345678901234567890123";
        let result = canonicalize_workspace_path(&link, salt, SymlinkPolicy::Allow)
            .map_err(|e| e.to_string())?;

        assert!(result.input_differs_from_canonical());
        Ok(())
    }

    #[test]
    fn canonicalize_symlink_denied_by_default_policy() -> TestResult {
        let scratch = ScratchDir::new("canon-symlink-deny")?;
        let target = scratch.make_dir("real-project")?;
        let link = scratch.path().join("linked-project");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link)
            .map_err(|e| format!("symlink creation failed: {e}"))?;

        #[cfg(not(unix))]
        return Ok(()); // Skip on non-Unix

        let salt = b"test-salt-12345678901234567890123";
        let result = canonicalize_workspace_path(&link, salt, SymlinkPolicy::Deny);

        match result {
            Err(CanonicalizationError::SymlinkBlocked { input_path, .. }) => {
                assert_eq!(input_path, link);
                Ok(())
            }
            Ok(_) => Err("expected SymlinkBlocked error".to_string()),
            Err(e) => Err(format!("unexpected error: {e}")),
        }
    }

    #[test]
    fn canonicalize_nonexistent_path_fails() {
        let nonexistent = PathBuf::from("/nonexistent/path/that/does/not/exist");
        let salt = b"test-salt-12345678901234567890123";

        let result = canonicalize_workspace_path(&nonexistent, salt, SymlinkPolicy::Allow);

        assert!(matches!(
            result,
            Err(CanonicalizationError::CanonicalizeFailure { .. })
        ));
    }

    #[test]
    fn canonicalize_is_idempotent() -> TestResult {
        let scratch = ScratchDir::new("canon-idempotent")?;
        let project = scratch.make_dir("project")?;
        let salt = b"test-salt-12345678901234567890123";

        let first = canonicalize_workspace_path(&project, salt, SymlinkPolicy::Allow)
            .map_err(|e| e.to_string())?;

        let second = canonicalize_workspace_path(&first.canonical_path, salt, SymlinkPolicy::Allow)
            .map_err(|e| e.to_string())?;

        assert_eq!(first.canonical_path, second.canonical_path);
        assert_eq!(first.salted_hash, second.salted_hash);
        Ok(())
    }

    #[test]
    fn different_salts_produce_different_hashes() -> TestResult {
        let scratch = ScratchDir::new("canon-salt-diff")?;
        let project = scratch.make_dir("project")?;

        let salt1 = b"salt-one-123456789012345678901234";
        let salt2 = b"salt-two-123456789012345678901234";

        let result1 = canonicalize_workspace_path(&project, salt1, SymlinkPolicy::Allow)
            .map_err(|e| e.to_string())?;
        let result2 = canonicalize_workspace_path(&project, salt2, SymlinkPolicy::Allow)
            .map_err(|e| e.to_string())?;

        assert_ne!(result1.salted_hash, result2.salted_hash);
        Ok(())
    }

    #[test]
    fn platform_case_handling_is_consistent() {
        let handling = PlatformCaseHandling::current_platform();
        assert!(matches!(
            handling,
            PlatformCaseHandling::Preserve | PlatformCaseHandling::Lower
        ));
        assert!(!handling.as_str().is_empty());
    }

    #[test]
    fn canonicalization_error_codes_are_stable() {
        let err1 = CanonicalizationError::CanonicalizeFailure {
            path: PathBuf::from("/test"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        };
        assert_eq!(err1.code(), "workspace_canonicalize_failed");

        let err2 = CanonicalizationError::SymlinkBlocked {
            input_path: PathBuf::from("/link"),
            canonical_path: PathBuf::from("/real"),
            symlink_chain: vec![],
        };
        assert_eq!(err2.code(), "workspace_symlink_blocked");
    }

    #[test]
    fn canonicalization_error_has_repair_suggestions() {
        let err = CanonicalizationError::SymlinkBlocked {
            input_path: PathBuf::from("/link"),
            canonical_path: PathBuf::from("/real"),
            symlink_chain: vec![],
        };
        let repair = err.repair();
        assert!(repair.contains("--allow-symlink"));
    }
}
