//! Real Git fixtures for the production collector. No mocked Git stdout is used
//! by the repository tests; parser-only negatives exercise malformed framing.

use super::*;
use crate::core::memory::{
    RememberGitCaptureOptions, build_remember_git_capture_candidate,
    remember_git_capture_candidate_from_repo,
};
use std::fs;

type TestResult = Result<(), String>;

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("-c")
        .arg("commit.gpgsign=false")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("fixture Git exited {}", output.status));
    }
    Ok(output.stdout)
}

fn git_text(root: &Path, args: &[&str]) -> Result<String, String> {
    String::from_utf8(git(root, args)?)
        .map(|s| s.trim_end_matches('\n').to_owned())
        .map_err(|e| e.to_string())
}

fn fixture() -> Result<tempfile::TempDir, String> {
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    git(directory.path(), &["init", "--initial-branch=main"])?;
    git(directory.path(), &["config", "user.name", "CaptureFixture"])?;
    git(
        directory.path(),
        &["config", "user.email", "capture@example.invalid"],
    )?;
    git(directory.path(), &["config", "core.autocrlf", "false"])?;
    Ok(directory)
}

fn write(root: &Path, name: &str, content: &str) -> TestResult {
    fs::write(root.join(name), content).map_err(|e| e.to_string())
}

fn commit(root: &Path, message: &str) -> Result<String, String> {
    git(root, &["add", "--all"])?;
    git(root, &["commit", "-m", message])?;
    git_text(root, &["rev-parse", "HEAD"])
}

fn capture(
    root: &Path,
    mode: RememberGitCaptureMode,
    reference: Option<&str>,
) -> Result<crate::core::memory::RememberGitCaptureCandidate, String> {
    remember_git_capture_candidate_from_repo(&RememberGitCaptureOptions {
        workspace_path: root,
        mode,
        reference,
    })
    .map_err(|e| e.message())
}

#[test]
fn unborn_capture_includes_staged_and_unstaged_net_content_without_writes() -> TestResult {
    let directory = fixture()?;
    let root = directory.path();
    write(root, "first.rs", "pub fn staged_intermediate() {}\n")?;
    git(root, &["add", "--", "first.rs"])?;
    write(root, "first.rs", "pub fn final_working_content() {}\n")?;
    write(root, "staged-only.rs", "pub fn staged_only() {}\n")?;
    git(root, &["add", "--", "staged-only.rs"])?;
    write(root, "untracked.rs", "DO_NOT_CAPTURE_UNTRACKED\n")?;
    let index = fs::read(root.join(".git/index")).map_err(|e| e.to_string())?;
    let objects = git(root, &["count-objects", "-v"])?;
    let raw = diff_input(root, None).map_err(|e| e.message())?;
    assert_eq!(raw.changed_files, ["first.rs", "staged-only.rs"]);
    assert!(raw.diff_text.contains("+pub fn final_working_content()"));
    assert!(raw.diff_text.contains("+pub fn staged_only()"));
    assert!(!raw.diff_text.contains("staged_intermediate"));
    assert!(!raw.diff_text.contains("DO_NOT_CAPTURE_UNTRACKED"));
    assert_eq!(
        fs::read(root.join(".git/index")).map_err(|e| e.to_string())?,
        index
    );
    assert_eq!(git(root, &["count-objects", "-v"])?, objects);
    assert!(!root.join(".ee").exists());
    let candidate = capture(root, RememberGitCaptureMode::WorkingTree, None)?;
    assert_eq!(candidate.changed_files, raw.changed_files);
    assert!(candidate.content.contains("final_working_content"));
    Ok(())
}

#[test]
fn root_commit_and_precommit_worktree_have_the_same_patch() -> TestResult {
    let directory = fixture()?;
    let root = directory.path();
    write(root, "first.rs", "pub fn root_change() {}\n")?;
    git(root, &["add", "--", "first.rs"])?;
    let before = diff_input(root, None).map_err(|e| e.message())?;
    let oid = commit(root, "initial source")?;
    let after = commit_input(root, "HEAD").map_err(|e| e.message())?;
    assert_eq!(after.commit_sha.as_deref(), Some(oid.as_str()));
    assert_eq!(after.changed_files, before.changed_files);
    assert_eq!(after.diff_text, before.diff_text);
    assert_eq!(
        build_remember_git_capture_candidate(&after).diff_fingerprint,
        build_remember_git_capture_candidate(&before).diff_fingerprint
    );
    Ok(())
}

#[test]
fn staged_then_cancelled_worktree_edits_do_not_capture_intermediate_content() -> TestResult {
    let directory = fixture()?;
    let root = directory.path();
    write(root, "source.rs", "pub fn original() {}\n")?;
    commit(root, "baseline")?;
    write(root, "source.rs", "pub fn staged_but_cancelled() {}\n")?;
    git(root, &["add", "--", "source.rs"])?;
    write(root, "source.rs", "pub fn original() {}\n")?;
    let raw = diff_input(root, None).map_err(|e| e.message())?;
    assert!(raw.changed_files.is_empty());
    assert!(raw.diff_text.is_empty());
    Ok(())
}

#[test]
fn merge_capture_uses_only_the_change_against_its_first_parent() -> TestResult {
    let directory = fixture()?;
    let root = directory.path();
    write(root, "base.rs", "pub fn base() {}\n")?;
    let base = commit(root, "baseline")?;
    // Branches exist only inside this throwaway fixture, never in the source
    // checkout. Independent, conflict-free changes make both parents distinct.
    git(root, &["switch", "-c", "fixture-side"])?;
    write(root, "side.rs", "pub fn side_change() {}\n")?;
    let side = commit(root, "side source")?;
    git(root, &["switch", "main"])?;
    write(root, "main.rs", "pub fn main_change() {}\n")?;
    let first_parent = commit(root, "main source")?;
    git(root, &["merge", "--no-ff", &side, "-m", "merge source"])?;
    let raw = commit_input(root, "HEAD").map_err(|e| e.message())?;
    assert_eq!(raw.changed_files, ["side.rs"]);
    assert!(raw.diff_text.contains("+pub fn side_change()"));
    assert!(!raw.diff_text.contains("main_change"));
    assert_eq!(
        raw.diff_text,
        comparison(root, &first_parent, raw.commit_sha.as_deref())
            .map_err(|e| e.message())?
            .1
    );
    assert_ne!(first_parent, base);
    let public = capture(root, RememberGitCaptureMode::Commit, Some("HEAD"))?;
    assert_eq!(public.changed_files, ["side.rs"]);
    Ok(())
}

#[test]
fn revision_ranges_do_not_accidentally_include_worktree_changes() -> TestResult {
    let directory = fixture()?;
    let root = directory.path();
    write(root, "base.rs", "pub fn base() {}\n")?;
    let base = commit(root, "base")?;
    write(root, "changed.rs", "pub fn committed_change() {}\n")?;
    let tip = commit(root, "change")?;
    write(root, "base.rs", "pub fn dirty_working_change() {}\n")?;
    let expected = comparison(root, &base, Some(&tip)).map_err(|e| e.message())?;
    for expression in [
        format!("{base}..{tip}"),
        format!("{base}...{tip}"),
        format!("{base}.."),
        format!("{base}..."),
    ] {
        let raw = diff_input(root, Some(&expression)).map_err(|e| e.message())?;
        assert_eq!((raw.changed_files, raw.diff_text), expected);
    }
    let live = diff_input(root, Some("HEAD")).map_err(|e| e.message())?;
    assert_eq!(live.changed_files, ["base.rs"]);
    assert!(live.diff_text.contains("dirty_working_change"));
    assert!(!live.diff_text.contains("committed_change"));
    Ok(())
}

#[test]
fn nul_paths_preserve_unicode_spaces_and_both_rename_endpoints() -> TestResult {
    let directory = fixture()?;
    let root = directory.path();
    let original = " leading 日本語.rs ";
    let renamed = "renamed 日本語.rs";
    write(root, original, "pub fn preserved() {}\n")?;
    commit(root, "baseline")?;
    git(root, &["mv", "--", original, renamed])?;
    let raw = diff_input(root, None).map_err(|e| e.message())?;
    assert_eq!(raw.changed_files, [original, renamed]);
    let public = build_remember_git_capture_candidate(&raw);
    assert_eq!(public.changed_files, [original, renamed]);
    assert!(
        !public.content.contains("ee-anchor:path:"),
        "whitespace-delimited anchors must not claim only part of either path"
    );
    Ok(())
}

#[test]
fn diff_presentation_settings_and_textconv_cannot_change_capture() -> TestResult {
    let directory = fixture()?;
    let root = directory.path();
    write(root, "source.rs", "pub fn before() {}\n")?;
    commit(root, "baseline")?;
    write(root, "source.rs", "pub fn after() {}\n")?;
    let expected = diff_input(root, None).map_err(|e| e.message())?;
    git(root, &["config", "color.ui", "always"])?;
    git(root, &["config", "diff.noprefix", "true"])?;
    git(root, &["config", "diff.mnemonicPrefix", "true"])?;
    git(root, &["config", "diff.algorithm", "histogram"])?;
    git(root, &["config", "diff.context", "0"])?;
    git(
        root,
        &["config", "diff.external", "ee-capture-must-not-run"],
    )?;
    git(
        root,
        &[
            "config",
            "diff.capture.textconv",
            "ee-textconv-must-not-run",
        ],
    )?;
    write(root, ".gitattributes", "*.rs diff=capture\n")?;
    let actual = diff_input(root, None).map_err(|e| e.message())?;
    assert_eq!(actual.changed_files, expected.changed_files);
    assert_eq!(actual.diff_text, expected.diff_text);
    assert!(!actual.diff_text.contains('\u{1b}'));
    Ok(())
}

#[test]
fn shallow_parent_is_not_misreported_as_a_root_commit() -> TestResult {
    let directory = fixture()?;
    let root = directory.path();
    write(root, "base.rs", "pub fn base() {}\n")?;
    let parent = commit(root, "base")?;
    write(root, "child.rs", "pub fn child() {}\n")?;
    let child = commit(root, "child")?;
    fs::write(root.join(".git/shallow"), format!("{child}\n")).map_err(|e| e.to_string())?;
    // The object header still has a parent even when history traversal hides
    // it. The parent is available here, so the exact delta can still be read.
    let raw = commit_input(root, "HEAD").map_err(|e| e.message())?;
    assert_eq!(raw.changed_files, ["child.rs"]);
    assert_eq!(
        raw.diff_text,
        comparison(root, &parent, Some(&child))
            .map_err(|e| e.message())?
            .1
    );
    Ok(())
}

#[test]
fn invalid_refs_and_unmerged_paths_refuse_before_building_a_candidate() -> TestResult {
    let directory = fixture()?;
    let root = directory.path();
    write(root, "file.rs", "pub fn base() {}\n")?;
    commit(root, "base")?;
    for expression in [
        "--stat",
        "HEAD with spaces",
        "HEAD..HEAD..HEAD",
        "missing-ref",
    ] {
        assert!(diff_input(root, Some(expression)).is_err());
    }
    git(root, &["switch", "-c", "fixture-conflict"])?;
    write(root, "file.rs", "pub fn one() {}\n")?;
    let side = commit(root, "side")?;
    git(root, &["switch", "main"])?;
    write(root, "file.rs", "pub fn two() {}\n")?;
    commit(root, "main")?;
    assert!(git(root, &["merge", &side]).is_err());
    let error = diff_input(root, None)
        .err()
        .ok_or("captured unresolved conflict")?;
    assert!(error.message().contains("unmerged"), "{}", error.message());
    // A committed comparison remains well-defined even when unrelated live
    // files are conflicted. Do not block historical capture on the worktree.
    let committed = commit_input(root, "HEAD").map_err(|e| e.message())?;
    assert!(committed.diff_text.contains("+pub fn two()"));
    assert!(!committed.diff_text.contains("<<<<<<<"));
    assert!(!root.join(".ee").exists());
    Ok(())
}

fn packet(status: &str, paths: &[&[u8]]) -> Vec<u8> {
    let mut output = format!(
        ":100644 100644 {} {} {status}\0",
        "0".repeat(40),
        "1".repeat(40)
    )
    .into_bytes();
    for path in paths {
        output.extend_from_slice(path);
        output.push(0);
    }
    output.extend_from_slice(b"\0diff --git a/file b/file\n");
    output
}

#[test]
fn raw_parser_uses_nul_framing_not_path_whitespace_or_patch_headers() -> TestResult {
    let (paths, patch) =
        parse_diff(&packet("R100", &[b"old\nname", b"new\tname"])).map_err(|e| e.message())?;
    assert_eq!(paths, ["new\tname", "old\nname"]);
    assert_eq!(patch, "diff --git a/file b/file\n");
    let (paths, _) = parse_diff(&packet("M", &[b":leading-colon"])).map_err(|e| e.message())?;
    assert_eq!(paths, [":leading-colon"]);
    for malformed in [
        packet("R101", &[b"old", b"new"]),
        packet("R", &[b"old"]),
        packet("X", &[b"file"]),
        packet("M", &[b"\xffPRIVATE_SENTINEL"]),
        packet("A", &[b""]),
        b"diff --git a/file b/file\n".to_vec(),
    ] {
        let error = parse_diff(&malformed)
            .err()
            .ok_or("accepted invalid raw diff")?;
        assert!(!error.message().contains("PRIVATE_SENTINEL"));
    }
    let complete = packet("M", &[b"file"]);
    for end in 1..complete
        .iter()
        .position(|byte| *byte == b'd')
        .ok_or("missing patch")?
    {
        assert!(
            parse_diff(&complete[..end]).is_err(),
            "accepted truncated frame at {end}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn literal_backslashes_and_newlines_never_become_different_file_anchors() -> TestResult {
    let directory = fixture()?;
    let root = directory.path();
    let path = "literal\\name.rs";
    write(root, path, "pub fn exact() {}\n")?;
    write(root, "multiline\nname.rs", "pub fn another() {}\n")?;
    git(root, &["add", "--all"])?;
    let candidate = capture(root, RememberGitCaptureMode::WorkingTree, None)?;
    assert_eq!(candidate.changed_files, [path]);
    assert!(
        !candidate
            .changed_files
            .iter()
            .any(|name| name == "literal/name.rs")
    );
    assert!(!candidate.content.contains("ee-anchor:path:"));
    Ok(())
}

#[test]
fn git_failures_scrub_stderr_without_echoing_capture_arguments() -> TestResult {
    let directory = fixture()?;
    let value = format!("ghp_{}", "q".repeat(36));
    let reference = format!("missing-{value}");
    let error = text(
        directory.path(),
        &["show", &reference, "--"],
        "read capture fixture",
    )
    .err()
    .ok_or("expected missing-ref failure")?;
    assert!(error.message().contains("read capture fixture"));
    assert!(error.message().contains("REDACTED"));
    assert!(!error.message().contains(&value));
    assert!(!error.message().contains(&reference));
    Ok(())
}

#[test]
fn sha256_root_capture_uses_the_repository_object_format_without_writing_objects() -> TestResult {
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = directory.path();
    git(
        root,
        &["init", "--object-format=sha256", "--initial-branch=main"],
    )?;
    git(root, &["config", "user.name", "CaptureFixture"])?;
    git(root, &["config", "user.email", "capture@example.invalid"])?;
    write(root, "initial.rs", "pub fn sha256_initial() {}\n")?;
    git(root, &["add", "--", "initial.rs"])?;
    let before = git(root, &["count-objects", "-v"])?;
    let raw = diff_input(root, None).map_err(|e| e.message())?;
    assert_eq!(raw.changed_files, ["initial.rs"]);
    assert_eq!(git(root, &["count-objects", "-v"])?, before);
    let oid = commit(root, "initial SHA256 source")?;
    assert_eq!(oid.len(), 64);
    let committed = commit_input(root, "HEAD").map_err(|e| e.message())?;
    assert_eq!(committed.commit_sha.as_deref(), Some(oid.as_str()));
    assert_eq!(raw.diff_text, committed.diff_text);
    Ok(())
}

#[test]
fn genuinely_missing_shallow_parent_refuses_instead_of_capturing_the_whole_repository() -> TestResult
{
    let source = fixture()?;
    write(source.path(), "base.rs", "pub fn historical_base() {}\n")?;
    commit(source.path(), "base")?;
    write(source.path(), "child.rs", "pub fn child_change() {}\n")?;
    commit(source.path(), "child")?;
    let target = tempfile::tempdir().map_err(|e| e.to_string())?;
    let source_path = source.path().to_str().ok_or("fixture path is not UTF-8")?;
    git(
        target.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "clone",
            "--no-local",
            "--depth=1",
            source_path,
            ".",
        ],
    )?;
    assert_eq!(
        git_text(target.path(), &["rev-parse", "--is-shallow-repository"])?,
        "true"
    );
    let index = fs::read(target.path().join(".git/index")).map_err(|e| e.to_string())?;
    let error = commit_input(target.path(), "HEAD")
        .err()
        .ok_or("accepted unavailable shallow parent")?;
    assert!(error.message().contains("read capture comparison"));
    assert_eq!(
        fs::read(target.path().join(".git/index")).map_err(|e| e.to_string())?,
        index
    );
    assert!(!target.path().join(".ee").exists());
    Ok(())
}

#[test]
fn large_multifile_capture_binds_the_last_change_beyond_its_visible_prefix() -> TestResult {
    let directory = fixture()?;
    let root = directory.path();
    write(root, "a.rs", "// original first file\n")?;
    write(root, "b.rs", "// original last file\n")?;
    commit(root, "baseline")?;
    write(root, "a.rs", &"// stable added context\n".repeat(2000))?;
    write(root, "b.rs", "pub fn last_alpha() {}\n")?;
    let first = diff_input(root, None).map_err(|e| e.message())?;
    write(root, "b.rs", "pub fn last_omega() {}\n")?;
    let second = diff_input(root, None).map_err(|e| e.message())?;
    let budget = super::super::REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES;
    assert!(first.diff_text.len() > budget && second.diff_text.len() > budget);
    // The previous hash-of-a-byte-prefix contract collapsed these real diffs.
    assert_eq!(&first.diff_text[..budget], &second.diff_text[..budget]);
    assert_eq!(first.changed_files, second.changed_files);
    let first = build_remember_git_capture_candidate(&first);
    let second = build_remember_git_capture_candidate(&second);
    assert_ne!(first.diff_fingerprint, second.diff_fingerprint);
    assert_ne!(first.source, second.source);
    assert!(!first.content.contains("last_alpha"));
    assert!(!second.content.contains("last_omega"));
    assert!(!root.join(".ee").exists());
    Ok(())
}

// A promisor remote can make a read write objects or block on network/auth.
// Use real filtered local clones so the missing-object arm cannot pass merely
// because the fixture already contains every blob it needs.
fn partial_fixture() -> Result<(tempfile::TempDir, tempfile::TempDir, Vec<String>), String> {
    let source = fixture()?;
    write(source.path(), "source.rs", "pub fn initial_release() {}\n")?;
    commit(source.path(), "initial source")?;
    write(source.path(), "source.rs", "pub fn verified_release() {}\n")?;
    commit(source.path(), "verify release")?;
    git(source.path(), &["config", "uploadpack.allowFilter", "true"])?;
    let objects = ["HEAD~1:source.rs", "HEAD:source.rs"]
        .into_iter()
        .map(|reference| git_text(source.path(), &["rev-parse", reference]))
        .collect::<Result<Vec<_>, _>>()?;
    let target = tempfile::tempdir().map_err(|error| error.to_string())?;
    let source_path = source.path().to_str().ok_or("non-UTF-8 fixture path")?;
    git(
        target.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "clone",
            "--no-local",
            "--filter=blob:none",
            "--no-checkout",
            source_path,
            ".",
        ],
    )?;
    assert_eq!(
        git_text(target.path(), &["config", "remote.origin.promisor"])?,
        "true"
    );
    for object in &objects {
        assert!(
            !object_present(target.path(), object)?,
            "filtered clone already has its blobs"
        );
    }
    Ok((source, target, objects))
}

fn object_present(root: &Path, object: &str) -> Result<bool, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["--no-lazy-fetch", "cat-file", "-e", object])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!("local object probe failed: {}", output.status)),
    }
}

#[test]
fn partial_clone_capture_refuses_missing_blobs_without_fetching_or_writing() -> TestResult {
    let (_source, target, objects) = partial_fixture()?;
    let root = target.path();
    let counts = git(root, &["count-objects", "-v"])?;
    assert!(!root.join(".git/index").exists());
    for (mode, reference) in [
        (RememberGitCaptureMode::Commit, Some("HEAD")),
        (RememberGitCaptureMode::Diff, Some("HEAD~1..HEAD")),
        (RememberGitCaptureMode::WorkingTree, None),
    ] {
        let error = capture(root, mode, reference)
            .err()
            .ok_or("fetched missing capture content")?;
        assert!(error.contains("read capture comparison"), "{error}");
        for object in &objects {
            assert!(
                !object_present(root, object)?,
                "capture silently fetched a missing blob"
            );
        }
        assert_eq!(git(root, &["count-objects", "-v"])?, counts);
        assert!(!root.join(".git/index").exists());
        assert!(!root.join(".ee").exists());
    }
    // Prove that the remote and lazy fetching actually work in this fixture.
    // Only this explicit fixture read is permitted to hydrate the objects.
    for object in &objects {
        git(root, &["cat-file", "blob", object])?;
        assert!(object_present(root, object)?);
    }
    assert_ne!(git(root, &["count-objects", "-v"])?, counts);
    Ok(())
}

#[test]
fn partial_clone_with_local_evidence_captures_offline_without_mutating_git() -> TestResult {
    let (source, target, objects) = partial_fixture()?;
    let root = target.path();
    for object in &objects {
        git(root, &["cat-file", "blob", object])?;
    }
    git(root, &["checkout", "HEAD", "--", "source.rs"])?;
    let unavailable = root.join("unavailable-promisor");
    git(
        root,
        &[
            "remote",
            "set-url",
            "origin",
            unavailable.to_str().ok_or("non-UTF-8 path")?,
        ],
    )?;
    let index = fs::read(root.join(".git/index")).map_err(|error| error.to_string())?;
    let counts = git(root, &["count-objects", "-v"])?;
    for (mode, reference) in [
        (RememberGitCaptureMode::Commit, Some("HEAD")),
        (RememberGitCaptureMode::Diff, Some("HEAD~1..HEAD")),
    ] {
        let captured = capture(root, mode, reference)?;
        let original = capture(source.path(), mode, reference)?;
        assert_eq!(captured, original);
        assert!(captured.content.contains("verified_release"));
    }
    write(root, "source.rs", "pub fn additional_offline_check() {}\n")?;
    let worktree = capture(root, RememberGitCaptureMode::WorkingTree, None)?;
    assert!(worktree.content.contains("additional_offline_check"));
    assert_eq!(
        fs::read(root.join(".git/index")).map_err(|error| error.to_string())?,
        index
    );
    assert_eq!(git(root, &["count-objects", "-v"])?, counts);
    assert!(!root.join(".ee").exists());
    Ok(())
}
