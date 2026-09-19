//! Git-backed capture with one path/patch result per comparison.
//!
//! Resolve revision expressions before reading their content. A merge is the
//! change against its first parent; a root (or unborn worktree) uses Git's own
//! empty tree for this repository's object format. Raw NUL-framed paths and
//! patch text come from the same diff invocation, never separate observations.
//! Working files can still change while Git reads them: this is not a filesystem
//! snapshot. The fingerprint belongs to the observed, sanitized patch.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::{
    RememberGitCaptureInput, RememberGitCaptureMode, remember_usage_error, validate_git_capture_ref,
};
use crate::models::DomainError;

// Explicit presentation and execution options prevent a repository's pager,
// textconv driver, diff helper, prefixes or color from changing captured bytes.
const DIFF_OPTIONS: &[&str] = &[
    "--raw",
    "-z",
    "--patch",
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
    "--find-renames=50%",
    "--unified=80",
    "--full-index",
    "--abbrev=64",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "--line-prefix=",
    "--no-relative",
    "--diff-algorithm=myers",
    "--no-indent-heuristic",
    "--submodule=short",
    "--ignore-submodules=none",
];

pub(super) fn root(workspace: &Path) -> Result<PathBuf, DomainError> {
    let value = text(
        workspace,
        &["rev-parse", "--show-toplevel"],
        "resolve git root",
    )?;
    // Strip the protocol terminator only. Whitespace can be part of a path.
    let value = value.strip_suffix('\n').unwrap_or(&value);
    if value.is_empty() {
        return Err(remember_usage_error(
            "git returned an empty repository root".to_owned(),
        ));
    }
    Ok(PathBuf::from(value))
}

pub(super) fn commit_input(
    root: &Path,
    reference: &str,
) -> Result<RememberGitCaptureInput, DomainError> {
    let reference = validate_git_capture_ref(reference)?;
    let commit = resolve(root, &reference, "commit")?;
    // Read the actual object header, not a history traversal that treats a
    // shallow boundary as a root. Missing shallow parents must cause a refusal,
    // not silently turn an ordinary commit into a full-repository addition.
    let object = bytes(root, &["cat-file", "commit", &commit], "read commit header")?;
    let parent = object
        .split(|byte| *byte == b'\n')
        .take_while(|line| !line.is_empty())
        .find_map(|line| line.strip_prefix(b"parent "))
        .map(parse_oid)
        .transpose()?;
    let base = match parent {
        Some(parent) => parent,
        None => empty_tree(root)?,
    };
    let message = text(
        root,
        &[
            "log",
            "-1",
            "--no-show-signature",
            "--format=%s%x00%b",
            &commit,
            "--",
        ],
        "read commit message",
    )?;
    let (subject, body) = message.split_once('\0').unwrap_or((&message, ""));
    let (changed_files, diff_text) = comparison(root, &base, Some(&commit))?;
    Ok(RememberGitCaptureInput {
        mode: RememberGitCaptureMode::Commit,
        reference: Some(reference),
        commit_sha: Some(commit),
        commit_subject: (!subject.trim().is_empty()).then(|| subject.trim().to_owned()),
        commit_body: (!body.trim().is_empty()).then(|| body.trim().to_owned()),
        changed_files,
        diff_text,
    })
}

pub(super) fn diff_input(
    root: &Path,
    reference: Option<&str>,
) -> Result<RememberGitCaptureInput, DomainError> {
    let reference = reference.map(validate_git_capture_ref).transpose()?;
    let (base, target) = match reference.as_deref() {
        Some(expression) => diff_endpoints(root, expression)?,
        None => (worktree_base(root)?, None),
    };
    let (changed_files, diff_text) = comparison(root, &base, target.as_deref())?;
    Ok(RememberGitCaptureInput {
        mode: if reference.is_some() {
            RememberGitCaptureMode::Diff
        } else {
            RememberGitCaptureMode::WorkingTree
        },
        reference,
        commit_sha: None,
        commit_subject: None,
        commit_body: None,
        changed_files,
        diff_text,
    })
}

fn diff_endpoints(root: &Path, expression: &str) -> Result<(String, Option<String>), DomainError> {
    // Preserve Git's existing two-dot and three-dot --from-diff behavior. Pin
    // BOTH endpoints once; never turn a committed range into a worktree diff.
    let range = expression
        .split_once("...")
        .map(|pair| (pair, true))
        .or_else(|| expression.split_once("..").map(|pair| (pair, false)));
    let Some(((left, right), merge_base)) = range else {
        return Ok((resolve(root, expression, "tree")?, None));
    };
    if left.contains("..") || right.contains("..") {
        return Err(remember_usage_error(
            "git capture expects one revision range".to_owned(),
        ));
    }
    let left = resolve(root, if left.is_empty() { "HEAD" } else { left }, "commit")?;
    let right = resolve(
        root,
        if right.is_empty() { "HEAD" } else { right },
        "commit",
    )?;
    let base = if merge_base {
        // Multiple best bases have no single unambiguous comparison. Refuse
        // rather than depend on Git's arbitrary first-base choice.
        let bases = text(
            root,
            &["merge-base", "--all", &left, &right],
            "resolve diff merge base",
        )?;
        let mut bases = bases.lines();
        let base = bases
            .next()
            .ok_or_else(|| protocol_error("missing merge base"))?;
        if bases.next().is_some() {
            return Err(remember_usage_error(
                "git capture has multiple merge bases; choose an explicit two-dot base".to_owned(),
            ));
        }
        parse_oid(base.as_bytes())?
    } else {
        left
    };
    Ok((base, Some(right)))
}

fn worktree_base(root: &Path) -> Result<String, DomainError> {
    let output = run(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            "HEAD^{commit}",
        ],
        "resolve HEAD",
    )?;
    if output.status.success() {
        return parse_oid(trim_lf(&output.stdout));
    }
    if output.status.code() != Some(1) {
        return Err(command_error(&output, "resolve HEAD"));
    }
    // A failed HEAD lookup alone is not evidence of an unborn repository.
    // Check that HEAD is symbolic and that its branch really does not exist.
    let branch = text(
        root,
        &["symbolic-ref", "--quiet", "HEAD"],
        "check unborn HEAD",
    )?;
    let branch = branch.strip_suffix('\n').unwrap_or(&branch);
    let exists = run(
        root,
        &["show-ref", "--verify", "--quiet", "--", branch],
        "check unborn branch",
    )?;
    if exists.status.code() != Some(1) {
        return Err(remember_usage_error(
            "git HEAD is not a readable commit; refusing to treat it as an empty repository"
                .to_owned(),
        ));
    }
    empty_tree(root)
}

fn empty_tree(root: &Path) -> Result<String, DomainError> {
    // stdin is null and -w is intentionally absent. This works for both SHA-1
    // and SHA-256 repositories without writing an object or touching the index.
    let output = bytes(
        root,
        &["hash-object", "-t", "tree", "--stdin"],
        "resolve empty tree",
    )?;
    parse_oid(trim_lf(&output))
}

fn resolve(root: &Path, reference: &str, kind: &str) -> Result<String, DomainError> {
    let expression = format!("{reference}^{{{kind}}}");
    let output = bytes(
        root,
        &["rev-parse", "--verify", "--end-of-options", &expression],
        "resolve capture revision",
    )?;
    parse_oid(trim_lf(&output))
}

fn comparison(
    root: &Path,
    base: &str,
    target: Option<&str>,
) -> Result<(Vec<String>, String), DomainError> {
    // Against a tree, Git reports a conflicted worktree as an ordinary M
    // patch, not necessarily a U record. Check the index explicitly instead
    // of allowing conflict markers to masquerade as resolved source evidence.
    // A committed comparison is independent of the caller's live index.
    if target.is_none() {
        refuse_unmerged(root)?;
    }
    let mut args = vec!["diff"];
    args.extend_from_slice(DIFF_OPTIONS);
    args.push(base);
    if let Some(target) = target {
        args.push(target);
    }
    args.push("--");
    let output = bytes(root, &args, "read capture comparison")?;
    if target.is_none() {
        // Also refuse a conflict introduced while Git was reading the files.
        // This is a conservative check, not a claim of an atomic filesystem
        // snapshot: concurrent ordinary edits remain possible.
        refuse_unmerged(root)?;
    }
    parse_diff(&output)
}

fn refuse_unmerged(root: &Path) -> Result<(), DomainError> {
    if !bytes(
        root,
        &["ls-files", "--unmerged", "-z", "--"],
        "check unresolved paths",
    )?
    .is_empty()
    {
        return Err(remember_usage_error(
            "git capture encountered unmerged paths; resolve conflicts before capture".to_owned(),
        ));
    }
    Ok(())
}

fn parse_diff(output: &[u8]) -> Result<(Vec<String>, String), DomainError> {
    if output.is_empty() {
        return Ok((Vec::new(), String::new()));
    }
    let mut rest = output;
    let mut paths = BTreeSet::new();
    loop {
        let header = field(&mut rest)?;
        if header.is_empty() {
            break;
        }
        let parts = header.split(|byte| *byte == b' ').collect::<Vec<_>>();
        if parts.len() != 5
            || parts[0].first() != Some(&b':')
            || !mode(&parts[0][1..])
            || !mode(parts[1])
            || parse_oid(parts[2]).is_err()
            || parse_oid(parts[3]).is_err()
        {
            return Err(protocol_error("invalid raw diff header"));
        }
        let status = parts[4];
        let Some(&kind) = status.first() else {
            return Err(protocol_error("missing diff status"));
        };
        if kind == b'U' {
            return Err(remember_usage_error(
                "git capture encountered unmerged paths; resolve conflicts before capture"
                    .to_owned(),
            ));
        }
        if !matches!(kind, b'A' | b'D' | b'M' | b'R' | b'C' | b'T')
            || (status.len() > 1 && !matches!(kind, b'M' | b'R' | b'C'))
            || (matches!(kind, b'R' | b'C') && status.len() == 1)
            || (status.len() > 1
                && (status.len() > 4
                    || !status[1..].iter().all(u8::is_ascii_digit)
                    || std::str::from_utf8(&status[1..])
                        .ok()
                        .and_then(|n| n.parse::<u8>().ok())
                        .is_none_or(|n| n > 100)))
        {
            return Err(protocol_error("invalid diff status"));
        }
        for _ in 0..if matches!(kind, b'R' | b'C') { 2 } else { 1 } {
            let path = std::str::from_utf8(field(&mut rest)?)
                .map_err(|_| protocol_error("a changed path is not UTF-8"))?;
            if path.is_empty() {
                return Err(protocol_error("empty changed path"));
            }
            paths.insert(path.to_owned());
        }
    }
    if paths.is_empty() || !rest.starts_with(b"diff --git ") {
        return Err(protocol_error("missing patch for changed paths"));
    }
    let patch = std::str::from_utf8(rest).map_err(|_| protocol_error("patch text is not UTF-8"))?;
    Ok((paths.into_iter().collect(), patch.to_owned()))
}

fn mode(value: &[u8]) -> bool {
    value.len() == 6 && value.iter().all(|byte| (b'0'..=b'7').contains(byte))
}

fn field<'a>(rest: &mut &'a [u8]) -> Result<&'a [u8], DomainError> {
    let input = *rest;
    let end = input
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| protocol_error("unterminated raw diff field"))?;
    let value = &input[..end];
    *rest = &input[end + 1..];
    Ok(value)
}

fn parse_oid(bytes: &[u8]) -> Result<String, DomainError> {
    if !matches!(bytes.len(), 40 | 64)
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(protocol_error("invalid resolved object identifier"));
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| protocol_error("invalid object identifier"))
}

fn trim_lf(value: &[u8]) -> &[u8] {
    value.strip_suffix(b"\n").unwrap_or(value)
}

fn protocol_error(detail: &str) -> DomainError {
    remember_usage_error(format!("Cannot capture Git evidence: {detail}"))
}

fn run(root: &Path, args: &[&str], phase: &'static str) -> Result<Output, DomainError> {
    Command::new("git")
        .args([
            "--no-pager",
            "--no-replace-objects",
            "-c",
            "core.quotePath=false",
        ])
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| DomainError::Configuration {
            message: format!("Failed to run git while trying to {phase}: {error}"),
            repair: Some("Install git and run this command inside a git workspace.".to_owned()),
        })
}

fn command_error(output: &Output, phase: &str) -> DomainError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let public = crate::policy::redact_public_replay_text(stderr.trim());
    remember_usage_error(format!(
        "git failed while trying to {phase} ({}): {}",
        output.status, public.content,
    ))
}

fn bytes(root: &Path, args: &[&str], phase: &'static str) -> Result<Vec<u8>, DomainError> {
    let output = run(root, args, phase)?;
    if !output.status.success() {
        return Err(command_error(&output, phase));
    }
    Ok(output.stdout)
}

fn text(root: &Path, args: &[&str], phase: &'static str) -> Result<String, DomainError> {
    String::from_utf8(bytes(root, args, phase)?)
        .map_err(|_| protocol_error("Git text output is not UTF-8"))
}

#[cfg(test)]
#[path = "memory_git_capture_repo_tests.rs"]
mod tests;
