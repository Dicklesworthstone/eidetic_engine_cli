//! bd-1eq3l.9 — Read-only Git porcelain v2 snapshot provider.
//!
//! The workspace-hygiene classifier (bd-1eq3l.1) needs a deterministic
//! view of the working tree before it can classify anything. Several
//! modules ad-hoc-shelling out to `git status` would make the feature
//! inconsistent and hard to prove read-only. This module is the one
//! place that talks to `git`, and it talks only to read paths:
//!
//! ```text
//! git status --porcelain=v2 --branch --untracked-files=all --ignored=no
//! git rev-parse HEAD                            (best-effort; no failure on missing HEAD)
//! ```
//!
//! What you get is a normalized [`WorkspacePorcelainSnapshot`] with:
//!
//! - `repo_root` (the discovered root of the repo, NOT the caller's CWD)
//! - `branch`, `head_sha` (best-effort; both `None` in detached/empty repos)
//! - `entries: Vec<PorcelainEntry>` sorted byte-stable by `(path, state.rank())`
//! - `generated_at` (RFC 3339, nanosecond precision; volatile under J7 strip-fields)
//!
//! Every entry carries `state` (one of the explicit `EntryState` variants),
//! optional `original_path` for renames/copies, and an `EntryMetadata` row
//! with `is_symlink`, `is_binary_or_large`, `size_bytes`, and
//! `symlink_target_inside_repo`. Filesystem metadata reads are bounded
//! at [`PORCELAIN_MAX_BYTES_FOR_BINARY_CHECK`] bytes; anything larger or
//! identified as binary by NUL-byte sniff is reported with metadata only
//! (no content scan).
//!
//! ## Hard contract — read-only
//!
//! - No `git` subcommand that can mutate the index, working tree, refs,
//!   stash, worktrees, branches, or remotes is ever invoked.
//! - No `fs::*` mutation API is ever invoked. Only `symlink_metadata`,
//!   `read_link`, `metadata`, and a bounded `read` for the binary-sniff
//!   prefix.
//! - When git is unavailable or the workspace is not inside a repo, the
//!   provider returns a `PorcelainSnapshotKind::NotARepository` snapshot
//!   with empty entries rather than failing — callers can degrade
//!   gracefully.
//!
//! ## Determinism
//!
//! Same git tree + same filesystem → byte-identical entries (modulo the
//! volatile `generated_at` field, which J7 strips before hash compare).
//! Ordering is `(path lexicographic, state rank)`; rename pairs report
//! `path = destination` so they sort with the destination.

use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// JSON schema constant for the porcelain snapshot artifact. Stable
/// across binary minor versions; bumping the suffix signals a breaking
/// shape change.
pub const WORKSPACE_PORCELAIN_SNAPSHOT_SCHEMA_V1: &str = "ee.workspace_porcelain_snapshot.v1";

/// Maximum file size in bytes to consider for the binary-sniff prefix
/// read. Anything larger is reported as `is_binary_or_large = true`
/// with no content scan. Keeps the snapshot bounded under huge LFS
/// fixtures.
pub const PORCELAIN_MAX_BYTES_FOR_BINARY_CHECK: u64 = 4 * 1024 * 1024;

/// Bytes of the prefix read for the NUL-byte binary sniff.
const PORCELAIN_BINARY_SNIFF_PREFIX_BYTES: usize = 8 * 1024;

/// Stable, byte-encoded canonical state for a porcelain entry. The
/// vocabulary deliberately enumerates the cases the bd-1eq3l.* classifier
/// needs rather than echoing the raw porcelain XY pair — callers should
/// not have to re-decode git's two-letter codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    Untracked,
    StagedAdded,
    StagedModified,
    StagedDeleted,
    StagedRenamed,
    StagedCopied,
    UnstagedModified,
    UnstagedDeleted,
    StagedAndUnstagedModified,
    Conflicted,
    Ignored,
}

impl EntryState {
    /// Stable ordering rank used as the secondary key when the same path
    /// has multiple state rows (e.g. a renamed file may surface under
    /// both source and destination).
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::StagedRenamed => 0,
            Self::StagedCopied => 1,
            Self::StagedAdded => 2,
            Self::StagedModified => 3,
            Self::StagedAndUnstagedModified => 4,
            Self::StagedDeleted => 5,
            Self::UnstagedModified => 6,
            Self::UnstagedDeleted => 7,
            Self::Conflicted => 8,
            Self::Untracked => 9,
            Self::Ignored => 10,
        }
    }

    /// Short kebab-case identifier used in JSON output and bead audit
    /// fields. Mirrors the `#[serde(rename_all = "snake_case")]` derive.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Untracked => "untracked",
            Self::StagedAdded => "staged_added",
            Self::StagedModified => "staged_modified",
            Self::StagedDeleted => "staged_deleted",
            Self::StagedRenamed => "staged_renamed",
            Self::StagedCopied => "staged_copied",
            Self::UnstagedModified => "unstaged_modified",
            Self::UnstagedDeleted => "unstaged_deleted",
            Self::StagedAndUnstagedModified => "staged_and_unstaged_modified",
            Self::Conflicted => "conflicted",
            Self::Ignored => "ignored",
        }
    }
}

/// Metadata snapshot for one porcelain entry. All fields are
/// best-effort: a path that no longer exists on disk (deleted entry)
/// reports `size_bytes = None` and `is_symlink = false`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryMetadata {
    pub is_symlink: bool,
    /// `true` when the file is over the size threshold or contains a
    /// NUL byte in the sniff prefix. Either condition exempts the file
    /// from policy-layer content scanning.
    pub is_binary_or_large: bool,
    pub size_bytes: Option<u64>,
    /// When `is_symlink` is true and the target resolves under
    /// `repo_root`, this is the canonical target path. Used downstream
    /// to detect symlinks that point outside the workspace (which the
    /// classifier flags as elevated-risk).
    pub symlink_target_inside_repo: Option<PathBuf>,
}

/// One row in the porcelain snapshot. Path is always normalized to a
/// forward-slash, repo-root-relative string.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PorcelainEntry {
    pub path: String,
    /// Set for renames/copies. The destination is in `path`; the source
    /// in `original_path`. Same byte-stable string format.
    pub original_path: Option<String>,
    pub state: EntryState,
    pub metadata: EntryMetadata,
}

/// Why a snapshot might be a no-op (used by callers to degrade
/// gracefully without confusing an empty snapshot for a clean repo).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PorcelainSnapshotKind {
    Repository,
    NotARepository,
    GitUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePorcelainSnapshot {
    pub schema: String,
    pub kind: PorcelainSnapshotKind,
    pub repo_root: Option<PathBuf>,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub entries: Vec<PorcelainEntry>,
    pub generated_at: String,
}

impl WorkspacePorcelainSnapshot {
    /// Empty sentinel returned when git is unavailable or the workspace
    /// is not in a git repo. Callers should treat this as "no signal"
    /// rather than "clean repo" — that's what `kind` is for.
    fn empty(kind: PorcelainSnapshotKind, repo_root: Option<PathBuf>) -> Self {
        Self {
            schema: WORKSPACE_PORCELAIN_SNAPSHOT_SCHEMA_V1.to_owned(),
            kind,
            repo_root,
            branch: None,
            head_sha: None,
            entries: Vec::new(),
            generated_at: crate::obs::now_rfc3339_nanos(),
        }
    }
}

#[derive(Debug)]
pub enum PorcelainError {
    /// `git status` returned a non-zero exit with stderr content that
    /// suggests the call itself was malformed (not "not a repo" — that
    /// degrades into `NotARepository`).
    GitInvocationFailed {
        stderr: String,
        exit_code: Option<i32>,
    },
    /// The porcelain v2 stream was syntactically malformed. Caller
    /// should fall back to a `NotARepository` snapshot and surface a
    /// degraded code; never panic.
    ParseError { line_index: usize, reason: String },
    /// I/O error during filesystem metadata read for a porcelain entry.
    /// Wrapped so the caller can produce a degraded code without
    /// leaking raw os-error strings.
    MetadataError {
        path: PathBuf,
        kind: ErrorKind,
        message: String,
    },
}

impl std::fmt::Display for PorcelainError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitInvocationFailed { stderr, exit_code } => write!(
                formatter,
                "git status invocation failed (exit {exit_code:?}): {stderr}"
            ),
            Self::ParseError { line_index, reason } => write!(
                formatter,
                "porcelain v2 parse error at entry {line_index}: {reason}"
            ),
            Self::MetadataError {
                path,
                kind,
                message,
            } => write!(
                formatter,
                "metadata read failed for {} ({kind:?}): {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for PorcelainError {}

const PORCELAIN_TRACE_SURFACE: &str = "workspace_porcelain_snapshot";

fn trace_porcelain(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "workspace_porcelain_collector",
        request_id = "workspace_porcelain_request",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-1eq3l.9"),
        surface = PORCELAIN_TRACE_SURFACE,
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "porcelain snapshot checkpoint"
    );
}

/// Top-level collector. Discover the repo root upward from `workspace`,
/// run the read-only porcelain v2 + rev-parse commands, parse, and
/// normalize.
///
/// Degrades to an empty `NotARepository` snapshot when:
/// - `workspace` (or any ancestor) is not inside a git repo
/// - the `git` binary is not on PATH or refuses to launch
///
/// Returns an error only when porcelain itself was syntactically
/// malformed (i.e. git ran but produced output we cannot trust). All
/// other failure modes degrade.
pub fn collect_porcelain_snapshot(
    workspace: &Path,
) -> Result<WorkspacePorcelainSnapshot, PorcelainError> {
    let started = Instant::now();
    trace_porcelain("input", 0, &[]);

    let repo_root = match discover_repo_root(workspace) {
        Some(root) => root,
        None => {
            trace_porcelain("response", elapsed_ms_since(started), &["not_a_repository"]);
            return Ok(WorkspacePorcelainSnapshot::empty(
                PorcelainSnapshotKind::NotARepository,
                None,
            ));
        }
    };

    let status_output = match run_git_status_porcelain(&repo_root) {
        Ok(stdout) => stdout,
        Err(GitInvocationOutcome::GitUnavailable) => {
            trace_porcelain("response", elapsed_ms_since(started), &["git_unavailable"]);
            return Ok(WorkspacePorcelainSnapshot::empty(
                PorcelainSnapshotKind::GitUnavailable,
                Some(repo_root),
            ));
        }
        Err(GitInvocationOutcome::NotARepository) => {
            trace_porcelain("response", elapsed_ms_since(started), &["not_a_repository"]);
            return Ok(WorkspacePorcelainSnapshot::empty(
                PorcelainSnapshotKind::NotARepository,
                Some(repo_root),
            ));
        }
        Err(GitInvocationOutcome::Failed { stderr, exit_code }) => {
            return Err(PorcelainError::GitInvocationFailed { stderr, exit_code });
        }
    };

    trace_porcelain("dependency_check", elapsed_ms_since(started), &[]);

    let parsed = parse_porcelain_v2(&status_output)?;
    let (branch, head_sha) = (parsed.branch, parsed.head_sha);
    let mut entries = collect_entries_with_metadata(&repo_root, parsed.rows)?;
    entries.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.state.rank().cmp(&right.state.rank()))
    });

    trace_porcelain("persistence", elapsed_ms_since(started), &[]);

    let snapshot = WorkspacePorcelainSnapshot {
        schema: WORKSPACE_PORCELAIN_SNAPSHOT_SCHEMA_V1.to_owned(),
        kind: PorcelainSnapshotKind::Repository,
        repo_root: Some(repo_root),
        branch,
        head_sha,
        entries,
        generated_at: crate::obs::now_rfc3339_nanos(),
    };
    trace_porcelain("response", elapsed_ms_since(started), &[]);
    Ok(snapshot)
}

fn elapsed_ms_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Walk upward from `start` looking for a `.git/` directory or file
/// (the file form is a git worktree). Stops at the filesystem root.
/// Returns the parent of the `.git` entry, which is the repo working
/// directory by convention.
fn discover_repo_root(start: &Path) -> Option<PathBuf> {
    let mut canonical = match start.canonicalize() {
        Ok(path) => path,
        Err(_) => start.to_path_buf(),
    };
    loop {
        let git_path = canonical.join(".git");
        if git_path.exists() {
            return Some(canonical);
        }
        if !canonical.pop() {
            return None;
        }
    }
}

enum GitInvocationOutcome {
    GitUnavailable,
    NotARepository,
    Failed {
        stderr: String,
        exit_code: Option<i32>,
    },
}

fn run_git_status_porcelain(repo_root: &Path) -> Result<String, GitInvocationOutcome> {
    // Read-only invocation. We intentionally omit `--ignored` because
    // listing every ignored file in a large repo blows up the snapshot
    // size; ignored files are reported through a separate per-path
    // check in the classifier when policy needs them.
    let output = match Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("status")
        .arg("--porcelain=v2")
        .arg("--branch")
        .arg("--untracked-files=all")
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(GitInvocationOutcome::GitUnavailable);
        }
        Err(error) => {
            return Err(GitInvocationOutcome::Failed {
                stderr: error.to_string(),
                exit_code: None,
            });
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stderr_lower = stderr.to_lowercase();
        if stderr_lower.contains("not a git repository") || stderr_lower.contains("could not find")
        {
            return Err(GitInvocationOutcome::NotARepository);
        }
        return Err(GitInvocationOutcome::Failed {
            stderr,
            exit_code: output.status.code(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Internal parsed shape — `rows` carries the per-entry porcelain v2
/// records without filesystem metadata yet; the collector fills that
/// in afterwards.
struct ParsedPorcelain {
    branch: Option<String>,
    head_sha: Option<String>,
    rows: Vec<PorcelainRow>,
}

#[derive(Clone)]
struct PorcelainRow {
    state: EntryState,
    path: String,
    original_path: Option<String>,
}

fn parse_porcelain_v2(raw: &str) -> Result<ParsedPorcelain, PorcelainError> {
    let mut branch = None;
    let mut head_sha = None;
    let mut rows = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let mut tokens = line.splitn(2, ' ');
        let header = tokens.next().unwrap_or("");
        let rest = tokens.next().unwrap_or("");
        match header {
            "#" => {
                if let Some(field) = rest.strip_prefix("branch.head ") {
                    let value = field.trim();
                    if value != "(detached)" && !value.is_empty() {
                        branch = Some(value.to_owned());
                    }
                } else if let Some(field) = rest.strip_prefix("branch.oid ") {
                    let value = field.trim();
                    if value != "(initial)" && !value.is_empty() {
                        head_sha = Some(value.to_owned());
                    }
                }
                // Other `#` rows (ab/upstream/etc.) are informational.
            }
            "1" => rows.push(parse_porcelain_one_byte(rest).map_err(|reason| {
                PorcelainError::ParseError {
                    line_index: index,
                    reason,
                }
            })?),
            "2" => rows.push(parse_porcelain_two_byte(rest).map_err(|reason| {
                PorcelainError::ParseError {
                    line_index: index,
                    reason,
                }
            })?),
            "u" => rows.push(PorcelainRow {
                state: EntryState::Conflicted,
                path: porcelain_tail_path(rest).map_err(|reason| PorcelainError::ParseError {
                    line_index: index,
                    reason,
                })?,
                original_path: None,
            }),
            "?" => rows.push(PorcelainRow {
                state: EntryState::Untracked,
                path: rest.to_owned(),
                original_path: None,
            }),
            "!" => rows.push(PorcelainRow {
                state: EntryState::Ignored,
                path: rest.to_owned(),
                original_path: None,
            }),
            _ => {
                // Forward-compat: unknown record types are skipped
                // rather than failing the snapshot.
            }
        }
    }
    Ok(ParsedPorcelain {
        branch,
        head_sha,
        rows,
    })
}

/// Parse a "1 XY ..." ordinary changed entry. The interesting tokens
/// are columns 0-1 (XY two-letter state) and the trailing path. Other
/// columns (file modes, OIDs) are ignored at this layer — the
/// classifier doesn't need them.
fn parse_porcelain_one_byte(rest: &str) -> Result<PorcelainRow, String> {
    let mut parts = rest.splitn(9, ' ');
    let xy = parts.next().ok_or_else(|| "missing XY".to_owned())?;
    // Skip the 7 metadata columns (sub, modeH, modeI, modeW, hashH, hashI, path).
    for _ in 0..7 {
        parts.next();
    }
    let path = parts
        .next()
        .ok_or_else(|| "missing path".to_owned())?
        .to_owned();
    Ok(PorcelainRow {
        state: state_from_xy(xy)?,
        path,
        original_path: None,
    })
}

/// Parse a "2 XY ..." rename/copy entry. Path token is
/// `<dest>\t<source>` per porcelain v2 spec.
fn parse_porcelain_two_byte(rest: &str) -> Result<PorcelainRow, String> {
    let mut parts = rest.splitn(10, ' ');
    let xy = parts.next().ok_or_else(|| "missing XY".to_owned())?;
    // Skip 8 metadata columns (sub, modeH, modeI, modeW, hashH, hashI, score, path).
    for _ in 0..8 {
        parts.next();
    }
    let path_tail = parts
        .next()
        .ok_or_else(|| "missing rename path tail".to_owned())?;
    let (dest, source) = path_tail
        .split_once('\t')
        .ok_or_else(|| "rename entry missing tab-separated source path".to_owned())?;
    Ok(PorcelainRow {
        state: state_from_xy(xy)?,
        path: dest.to_owned(),
        original_path: Some(source.to_owned()),
    })
}

/// Strip the leading metadata columns from a `u`-record (unmerged) and
/// return only the path. Unmerged entries carry 10 metadata columns
/// before the path per porcelain v2.
fn porcelain_tail_path(rest: &str) -> Result<String, String> {
    let mut parts = rest.splitn(11, ' ');
    for _ in 0..10 {
        parts.next();
    }
    parts
        .next()
        .map(str::to_owned)
        .ok_or_else(|| "missing unmerged path".to_owned())
}

/// Map the porcelain v2 XY two-letter state code to the explicit
/// [`EntryState`] vocabulary. Recognized codes:
///
/// | XY  | Meaning                                       |
/// |-----|-----------------------------------------------|
/// | A.  | staged added                                  |
/// | M.  | staged modified                               |
/// | D.  | staged deleted                                |
/// | R.  | staged renamed                                |
/// | C.  | staged copied                                 |
/// | .M  | unstaged modified                             |
/// | .D  | unstaged deleted                              |
/// | MM  | staged AND unstaged modified                  |
/// | AM  | staged add with later working-tree edits      |
/// | RM  | staged rename with later working-tree edits   |
fn state_from_xy(xy: &str) -> Result<EntryState, String> {
    if xy.len() != 2 {
        return Err(format!("expected 2-char XY state, got {xy:?}"));
    }
    let bytes = xy.as_bytes();
    Ok(match (bytes[0], bytes[1]) {
        (b'A', b'.') | (b'A', b' ') => EntryState::StagedAdded,
        (b'A', b'M') => EntryState::StagedAndUnstagedModified,
        (b'M', b'.') | (b'M', b' ') => EntryState::StagedModified,
        (b'M', b'M') => EntryState::StagedAndUnstagedModified,
        (b'D', b'.') | (b'D', b' ') => EntryState::StagedDeleted,
        (b'R', b'.') | (b'R', b' ') => EntryState::StagedRenamed,
        (b'R', b'M') => EntryState::StagedAndUnstagedModified,
        (b'C', b'.') | (b'C', b' ') => EntryState::StagedCopied,
        (b'.', b'M') | (b' ', b'M') => EntryState::UnstagedModified,
        (b'.', b'D') | (b' ', b'D') => EntryState::UnstagedDeleted,
        _ => return Err(format!("unknown XY state {xy:?}")),
    })
}

fn collect_entries_with_metadata(
    repo_root: &Path,
    rows: Vec<PorcelainRow>,
) -> Result<Vec<PorcelainEntry>, PorcelainError> {
    let mut entries = Vec::with_capacity(rows.len());
    // Symlink loop guard: track canonical paths we've already followed
    // so a `a -> b -> a` cycle doesn't recurse.
    let mut visited_symlinks: BTreeSet<PathBuf> = BTreeSet::new();
    for row in rows {
        let metadata = collect_entry_metadata(repo_root, &row.path, &mut visited_symlinks);
        entries.push(PorcelainEntry {
            path: normalize_path(&row.path),
            original_path: row.original_path.as_deref().map(normalize_path),
            state: row.state,
            metadata,
        });
    }
    Ok(entries)
}

fn normalize_path(raw: &str) -> String {
    raw.replace('\\', "/")
}

fn collect_entry_metadata(
    repo_root: &Path,
    relative: &str,
    visited_symlinks: &mut BTreeSet<PathBuf>,
) -> EntryMetadata {
    let abs = repo_root.join(relative);
    let symlink_meta = match fs::symlink_metadata(&abs) {
        Ok(meta) => meta,
        // Missing path is normal for deleted entries — return defaults.
        Err(error) if error.kind() == ErrorKind::NotFound => return EntryMetadata::default(),
        Err(_) => return EntryMetadata::default(),
    };
    if symlink_meta.file_type().is_symlink() {
        let target = match fs::read_link(&abs) {
            Ok(target) => target,
            Err(_) => {
                return EntryMetadata {
                    is_symlink: true,
                    ..EntryMetadata::default()
                };
            }
        };
        let resolved = if target.is_absolute() {
            target.clone()
        } else {
            abs.parent().map(|p| p.join(&target)).unwrap_or(target)
        };
        let canonical = resolved.canonicalize().ok();
        let inside_repo = canonical.as_ref().and_then(|canon| {
            if canon.starts_with(repo_root) {
                Some(canon.clone())
            } else {
                None
            }
        });
        if let Some(ref canon) = inside_repo {
            if visited_symlinks.contains(canon) {
                // Loop — return symlink-only metadata.
                return EntryMetadata {
                    is_symlink: true,
                    symlink_target_inside_repo: inside_repo,
                    ..EntryMetadata::default()
                };
            }
            visited_symlinks.insert(canon.clone());
        }
        return EntryMetadata {
            is_symlink: true,
            is_binary_or_large: false,
            size_bytes: None,
            symlink_target_inside_repo: inside_repo,
        };
    }
    let size_bytes = if symlink_meta.is_file() {
        Some(symlink_meta.len())
    } else {
        None
    };
    let is_binary_or_large = match size_bytes {
        Some(size) if size > PORCELAIN_MAX_BYTES_FOR_BINARY_CHECK => true,
        Some(_) if symlink_meta.is_file() => is_binary_via_prefix_sniff(&abs),
        _ => false,
    };
    EntryMetadata {
        is_symlink: false,
        is_binary_or_large,
        size_bytes,
        symlink_target_inside_repo: None,
    }
}

/// Sniff the first `PORCELAIN_BINARY_SNIFF_PREFIX_BYTES` for a NUL byte.
/// Cheap, deterministic, doesn't pull the full file. Conservatively
/// returns `false` (text) on any I/O error so we don't accidentally
/// hide actual changes from the classifier — false positives only.
fn is_binary_via_prefix_sniff(path: &Path) -> bool {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut prefix = vec![0_u8; PORCELAIN_BINARY_SNIFF_PREFIX_BYTES];
    let read = file.read(&mut prefix).unwrap_or(0);
    prefix.truncate(read);
    prefix.contains(&0_u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs as unix_fs;
    use std::path::Path;
    use std::process::Command;

    type TestResult = Result<(), String>;

    /// Initialize a git repository in `dir` with `git init -q -b main`.
    /// All tests run on isolated tempdirs so concurrent test runs can't
    /// interfere with each other.
    fn init_repo(dir: &Path) -> TestResult {
        run_git(dir, &["init", "-q", "-b", "main"])?;
        run_git(dir, &["config", "user.email", "test@example.com"])?;
        run_git(dir, &["config", "user.name", "Test"])?;
        // Make sure we're not following a global gitignore that might
        // hide test fixtures.
        run_git(dir, &["config", "core.excludesfile", "/dev/null"])?;
        Ok(())
    }

    fn run_git(dir: &Path, args: &[&str]) -> TestResult {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map_err(|error| format!("git {args:?}: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "git {args:?} failed: stderr={}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(())
    }

    fn worker_local_tempdir(prefix: &str) -> Result<tempfile::TempDir, String> {
        // Same Mac→Linux RCH portability pattern used in
        // tests/closure_lint_harness.rs::closure_lint_worker_local_tempdir
        // and tests/preflight_hook_{bash,zsh}.rs. Mac-side TMPDIR points
        // at /Volumes/USBNVME16TB/... which doesn't exist on RCH Linux
        // workers; rooting under /tmp avoids the os-error-2 panic.
        let tmp_root = Path::new("/tmp");
        if tmp_root.is_dir() {
            tempfile::Builder::new()
                .prefix(prefix)
                .tempdir_in(tmp_root)
                .map_err(|error| error.to_string())
        } else {
            tempfile::Builder::new()
                .prefix(prefix)
                .tempdir()
                .map_err(|error| error.to_string())
        }
    }

    fn write_file(dir: &Path, relative: &str, body: &[u8]) -> TestResult {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = fs::File::create(&path).map_err(|e| e.to_string())?;
        file.write_all(body).map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn clean_repo_returns_zero_entries() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-clean-")?;
        init_repo(temp.path())?;
        write_file(temp.path(), "README.md", b"# initial\n")?;
        run_git(temp.path(), &["add", "."])?;
        run_git(temp.path(), &["commit", "-q", "-m", "initial"])?;

        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        assert_eq!(snapshot.kind, PorcelainSnapshotKind::Repository);
        assert_eq!(snapshot.entries, vec![]);
        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert!(snapshot.head_sha.is_some());
        Ok(())
    }

    #[test]
    fn staged_added_file_is_reported() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-staged-add-")?;
        init_repo(temp.path())?;
        write_file(temp.path(), "README.md", b"# initial\n")?;
        run_git(temp.path(), &["add", "."])?;
        run_git(temp.path(), &["commit", "-q", "-m", "initial"])?;
        write_file(temp.path(), "src/a.rs", b"pub fn a() {}\n")?;
        run_git(temp.path(), &["add", "src/a.rs"])?;

        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].path, "src/a.rs");
        assert_eq!(snapshot.entries[0].state, EntryState::StagedAdded);
        Ok(())
    }

    #[test]
    fn unstaged_modified_file_is_reported() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-unstaged-mod-")?;
        init_repo(temp.path())?;
        write_file(temp.path(), "src/a.rs", b"pub fn a() {}\n")?;
        run_git(temp.path(), &["add", "."])?;
        run_git(temp.path(), &["commit", "-q", "-m", "initial"])?;
        write_file(temp.path(), "src/a.rs", b"pub fn a() { /* changed */ }\n")?;

        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].path, "src/a.rs");
        assert_eq!(snapshot.entries[0].state, EntryState::UnstagedModified);
        Ok(())
    }

    #[test]
    fn staged_and_unstaged_same_file_is_reported_as_combined_state() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-staged-and-unstaged-")?;
        init_repo(temp.path())?;
        write_file(temp.path(), "src/a.rs", b"pub fn a() {}\n")?;
        run_git(temp.path(), &["add", "."])?;
        run_git(temp.path(), &["commit", "-q", "-m", "initial"])?;
        // Stage one change, then mutate again without staging.
        write_file(temp.path(), "src/a.rs", b"pub fn a() { 1 }\n")?;
        run_git(temp.path(), &["add", "src/a.rs"])?;
        write_file(temp.path(), "src/a.rs", b"pub fn a() { 2 }\n")?;

        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].path, "src/a.rs");
        assert_eq!(
            snapshot.entries[0].state,
            EntryState::StagedAndUnstagedModified
        );
        Ok(())
    }

    #[test]
    fn untracked_file_is_reported() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-untracked-")?;
        init_repo(temp.path())?;
        write_file(temp.path(), "README.md", b"# initial\n")?;
        run_git(temp.path(), &["add", "."])?;
        run_git(temp.path(), &["commit", "-q", "-m", "initial"])?;
        write_file(temp.path(), "scratch.txt", b"not for commit\n")?;

        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        let untracked: Vec<&PorcelainEntry> = snapshot
            .entries
            .iter()
            .filter(|e| e.state == EntryState::Untracked)
            .collect();
        assert_eq!(untracked.len(), 1);
        assert_eq!(untracked[0].path, "scratch.txt");
        Ok(())
    }

    #[test]
    fn rename_carries_original_path() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-rename-")?;
        init_repo(temp.path())?;
        write_file(temp.path(), "src/a.rs", b"pub fn a() { 0 }\n")?;
        run_git(temp.path(), &["add", "."])?;
        run_git(temp.path(), &["commit", "-q", "-m", "initial"])?;
        // git mv equivalent: physically move + git add -A.
        fs::rename(temp.path().join("src/a.rs"), temp.path().join("src/b.rs"))
            .map_err(|e| e.to_string())?;
        run_git(temp.path(), &["add", "-A"])?;

        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        let renamed: Vec<&PorcelainEntry> = snapshot
            .entries
            .iter()
            .filter(|e| e.state == EntryState::StagedRenamed)
            .collect();
        assert_eq!(renamed.len(), 1);
        assert_eq!(renamed[0].path, "src/b.rs");
        assert_eq!(renamed[0].original_path.as_deref(), Some("src/a.rs"));
        Ok(())
    }

    #[test]
    fn staged_delete_is_reported() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-delete-")?;
        init_repo(temp.path())?;
        write_file(temp.path(), "src/a.rs", b"pub fn a() {}\n")?;
        run_git(temp.path(), &["add", "."])?;
        run_git(temp.path(), &["commit", "-q", "-m", "initial"])?;
        fs::remove_file(temp.path().join("src/a.rs")).map_err(|e| e.to_string())?;
        run_git(temp.path(), &["add", "-A"])?;

        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].path, "src/a.rs");
        assert_eq!(snapshot.entries[0].state, EntryState::StagedDeleted);
        // Deleted file should have no size and no symlink metadata.
        assert_eq!(snapshot.entries[0].metadata.size_bytes, None);
        assert!(!snapshot.entries[0].metadata.is_symlink);
        Ok(())
    }

    #[test]
    fn conflict_entry_is_reported() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-conflict-")?;
        init_repo(temp.path())?;
        write_file(temp.path(), "src/a.rs", b"shared base\n")?;
        run_git(temp.path(), &["add", "."])?;
        run_git(temp.path(), &["commit", "-q", "-m", "base"])?;
        run_git(temp.path(), &["checkout", "-q", "-b", "feature"])?;
        write_file(temp.path(), "src/a.rs", b"feature side\n")?;
        run_git(temp.path(), &["commit", "-q", "-am", "feature"])?;
        run_git(temp.path(), &["checkout", "-q", "main"])?;
        write_file(temp.path(), "src/a.rs", b"main side\n")?;
        run_git(temp.path(), &["commit", "-q", "-am", "main"])?;
        // Merge intentionally produces a conflict.
        let _merge = Command::new("git")
            .arg("-C")
            .arg(temp.path())
            .args(["merge", "feature"])
            .output()
            .map_err(|e| e.to_string())?;

        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        let conflicted: Vec<&PorcelainEntry> = snapshot
            .entries
            .iter()
            .filter(|e| e.state == EntryState::Conflicted)
            .collect();
        assert_eq!(
            conflicted.len(),
            1,
            "expected one conflict; got {snapshot:?}"
        );
        assert_eq!(conflicted[0].path, "src/a.rs");
        Ok(())
    }

    #[test]
    fn ignored_files_are_excluded_by_default() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-ignored-")?;
        init_repo(temp.path())?;
        write_file(temp.path(), ".gitignore", b"build/\n")?;
        run_git(temp.path(), &["add", "."])?;
        run_git(temp.path(), &["commit", "-q", "-m", "init"])?;
        write_file(temp.path(), "build/output.bin", b"binary\n")?;

        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        // Should not appear — we do NOT pass --ignored, so the ignored
        // file is invisible to the porcelain output.
        assert!(
            snapshot
                .entries
                .iter()
                .all(|e| e.state != EntryState::Ignored)
        );
        assert!(
            snapshot
                .entries
                .iter()
                .all(|e| !e.path.starts_with("build/"))
        );
        Ok(())
    }

    #[test]
    fn symlink_metadata_is_captured_without_following() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-symlink-")?;
        init_repo(temp.path())?;
        write_file(temp.path(), "target.txt", b"real\n")?;
        unix_fs::symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
            .map_err(|e| e.to_string())?;

        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        let link_entry = snapshot
            .entries
            .iter()
            .find(|e| e.path == "link.txt")
            .ok_or_else(|| format!("symlink not in entries: {snapshot:?}"))?;
        assert!(link_entry.metadata.is_symlink);
        assert!(link_entry.metadata.symlink_target_inside_repo.is_some());
        // Symlink metadata must NOT have a content size — that would mean
        // we followed the link.
        assert_eq!(link_entry.metadata.size_bytes, None);
        Ok(())
    }

    #[test]
    fn binary_file_is_flagged_via_nul_byte_sniff() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-binary-")?;
        init_repo(temp.path())?;
        // Untracked binary file with NUL bytes in the first KB.
        let mut body = b"\0\0\0\0prefix-then-nuls".to_vec();
        body.extend_from_slice(&[0u8; 1024]);
        write_file(temp.path(), "blob.bin", &body)?;

        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        let blob = snapshot
            .entries
            .iter()
            .find(|e| e.path == "blob.bin")
            .ok_or_else(|| format!("binary blob not in entries: {snapshot:?}"))?;
        assert!(
            blob.metadata.is_binary_or_large,
            "NUL-prefixed file should be flagged as binary; got {blob:?}"
        );
        Ok(())
    }

    #[test]
    fn entries_are_sorted_byte_stable_by_path_then_state_rank() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-ordering-")?;
        init_repo(temp.path())?;
        write_file(temp.path(), "z.rs", b"z\n")?;
        write_file(temp.path(), "a.rs", b"a\n")?;
        write_file(temp.path(), "m.rs", b"m\n")?;
        // Leave them all untracked; ordering should be a, m, z.

        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        let paths: Vec<&str> = snapshot.entries.iter().map(|e| e.path.as_str()).collect();
        // Untracked entries only; just the three test files.
        let untracked: Vec<&str> = snapshot
            .entries
            .iter()
            .filter(|e| e.state == EntryState::Untracked)
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(untracked, vec!["a.rs", "m.rs", "z.rs"], "got {paths:?}");
        Ok(())
    }

    #[test]
    fn workspace_not_in_repo_degrades_to_not_a_repository() -> TestResult {
        let temp = worker_local_tempdir("ee-porcelain-nogit-")?;
        // No `git init` here — just an empty tempdir.
        let snapshot = collect_porcelain_snapshot(temp.path())
            .map_err(|error| format!("collect failed: {error}"))?;
        assert_eq!(snapshot.kind, PorcelainSnapshotKind::NotARepository);
        assert!(snapshot.entries.is_empty());
        assert!(snapshot.branch.is_none());
        assert!(snapshot.head_sha.is_none());
        Ok(())
    }

    #[test]
    fn entry_state_rank_is_total_order_with_no_ties() {
        // Verify the rank() table is injective so the (path, rank)
        // ordering is well-defined.
        let states = [
            EntryState::StagedRenamed,
            EntryState::StagedCopied,
            EntryState::StagedAdded,
            EntryState::StagedModified,
            EntryState::StagedAndUnstagedModified,
            EntryState::StagedDeleted,
            EntryState::UnstagedModified,
            EntryState::UnstagedDeleted,
            EntryState::Conflicted,
            EntryState::Untracked,
            EntryState::Ignored,
        ];
        let mut ranks: Vec<u8> = states.iter().map(|s| s.rank()).collect();
        ranks.sort_unstable();
        ranks.dedup();
        assert_eq!(ranks.len(), states.len(), "rank() must be injective");
    }
}
