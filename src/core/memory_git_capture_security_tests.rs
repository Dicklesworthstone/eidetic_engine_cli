//! Capture must remove actual values before budgeting, and its generated
//! metadata must not make already-sanitized evidence fail ordinary admission.

use super::*;
use std::process::Command;

type TestResult = Result<(), String>;

fn input(mode: RememberGitCaptureMode, diff_text: String) -> RememberGitCaptureInput {
    RememberGitCaptureInput {
        mode,
        reference: (mode != RememberGitCaptureMode::WorkingTree).then(|| "HEAD".to_owned()),
        commit_sha: (mode == RememberGitCaptureMode::Commit)
            .then(|| "0123456789abcdef0123456789abcdef01234567".to_owned()),
        commit_subject: None,
        commit_body: None,
        changed_files: vec!["src/capture.rs".to_owned()],
        diff_text,
    }
}

fn assert_policy_clean(candidate: &RememberGitCaptureCandidate) {
    let report = crate::policy::redact_secret_like_content(&candidate.content);
    assert!(
        !report.redacted,
        "generated {:?} capture fails ordinary admission: {:?}",
        candidate.mode, report.redacted_reasons,
    );
}

#[test]
fn generated_capture_metadata_does_not_treat_its_digest_as_a_credential() {
    // Pin the old failure independently of the candidate builder: this is a
    // public digest, but the old generated note supplies credential context.
    let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let old = format!(
        "Diff fingerprint: blake3:{digest}.\nMode: diff.\nRedaction: secret-like diff or message content was redacted before memory capture (openai_api_key)."
    );
    assert!(crate::policy::redact_secret_like_content(&old).redacted);
    let diff = format!("+let value = \"sk-proj-{}\";", "q".repeat(44));
    for mode in [
        RememberGitCaptureMode::Commit,
        RememberGitCaptureMode::Diff,
        RememberGitCaptureMode::WorkingTree,
    ] {
        let candidate = build_remember_git_capture_candidate(&input(mode, diff.clone()));
        assert!(candidate.redacted);
        assert_policy_clean(&candidate);
    }
}

#[test]
fn capture_modes_agree_for_detectable_subthreshold_and_mixed_values() {
    let detectable = format!("sk-proj-{}", "q".repeat(44));
    let below_shape_minimum = format!("sk-proj-{}", "p".repeat(39));
    for (values, should_redact) in [
        (vec![detectable.as_str()], true),
        (vec![below_shape_minimum.as_str()], false),
        (
            vec![detectable.as_str(), below_shape_minimum.as_str()],
            true,
        ),
    ] {
        let diff = values
            .iter()
            .enumerate()
            .map(|(index, value)| format!("+let value_{index} = \"{value}\";"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut fingerprints = BTreeSet::new();
        for mode in [
            RememberGitCaptureMode::Commit,
            RememberGitCaptureMode::Diff,
            RememberGitCaptureMode::WorkingTree,
        ] {
            let candidate = build_remember_git_capture_candidate(&input(mode, diff.clone()));
            assert_eq!(candidate.redacted, should_redact);
            assert!(!candidate.content.contains(&detectable));
            if values.contains(&below_shape_minimum.as_str()) {
                // Do not "repair" the mode mismatch by silently widening the
                // generic policy's explicitly shape-bounded detector.
                assert!(candidate.content.contains(&below_shape_minimum));
            }
            assert_policy_clean(&candidate);
            fingerprints.insert(candidate.diff_fingerprint);
        }
        assert_eq!(fingerprints.len(), 1, "same sanitized diff, same digest");
    }
}

#[test]
fn secret_crossing_the_capture_budget_is_redacted_before_truncation() {
    let token = format!("sk-proj-{}", "q".repeat(44));
    let visible_prefix = format!("sk-proj-{}", "q".repeat(12));
    for prefix in ["+ ", "+ 日本語 "] {
        let padding = REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES - prefix.len() - visible_prefix.len();
        let diff = format!(
            "{prefix}{}{token};\n+let safe_tail = true;",
            " ".repeat(padding)
        );
        let truncated_raw = truncate_utf8_lossless(&diff, REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES);
        // The unchanged detector cannot classify the prefix after destructive
        // truncation. This is why screening must precede presentation limits.
        assert!(!crate::policy::redact_secret_like_content(&truncated_raw).redacted);
        assert!(crate::policy::redact_secret_like_content(&diff).redacted);
        let candidate = build_remember_git_capture_candidate(&input(
            RememberGitCaptureMode::Diff,
            diff.clone(),
        ));
        assert!(candidate.redacted);
        assert!(!candidate.content.contains(&visible_prefix));
        assert!(!candidate.content.contains(&token));
        assert!(
            candidate
                .redaction_reasons
                .iter()
                .any(|reason| reason == "openai_api_key")
        );
        // The fingerprint now binds the full sanitized evidence, not the
        // bounded presentation prefix. Truncation still follows screening.
        let expected = crate::policy::redact_secret_like_content(&diff).content;
        assert_eq!(
            candidate.diff_fingerprint,
            format!("blake3:{}", blake3::hash(expected.as_bytes()).to_hex()),
        );
        assert_policy_clean(&candidate);
    }
}

#[test]
fn sanitized_git_capture_requires_no_policy_bypass_or_storage_creation() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    for mode in [
        RememberGitCaptureMode::Commit,
        RememberGitCaptureMode::Diff,
        RememberGitCaptureMode::WorkingTree,
    ] {
        let candidate = build_remember_git_capture_candidate(&input(
            mode,
            format!("+let value = \"sk-proj-{}\";", "q".repeat(44)),
        ));
        let bypass = validate_remember_policy(&candidate.content, root.path(), false)
            .map_err(|error| error.message())?;
        assert!(bypass.is_none());
        assert!(!root.path().join(".ee").exists());
    }
    Ok(())
}

fn git(path: &Path, arguments: &[&str]) -> TestResult {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("fixture git command failed: {}", output.status))
    }
}

#[test]
fn real_commit_diff_and_worktree_capture_share_the_same_admission_policy() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    git(root.path(), &["init", "--initial-branch=main"])?;
    git(root.path(), &["config", "user.name", "CaptureFixture"])?;
    git(
        root.path(),
        &["config", "user.email", "capture@example.invalid"],
    )?;
    let path = root.path().join("capture.rs");
    fs::write(&path, "fn keep_me() {}\n").map_err(|error| error.to_string())?;
    git(root.path(), &["add", "capture.rs"])?;
    git(
        root.path(),
        &["-c", "commit.gpgsign=false", "commit", "-m", "baseline"],
    )?;
    let token = format!("sk-proj-{}", "q".repeat(44));
    fs::write(
        &path,
        format!("fn keep_me() {{}}\nlet value = \"{token}\";\n"),
    )
    .map_err(|error| error.to_string())?;
    let worktree = remember_git_capture_candidate_from_repo(&RememberGitCaptureOptions {
        workspace_path: root.path(),
        mode: RememberGitCaptureMode::WorkingTree,
        reference: None,
    })
    .map_err(|error| error.message())?;
    git(root.path(), &["add", "capture.rs"])?;
    git(
        root.path(),
        &["-c", "commit.gpgsign=false", "commit", "-m", "capture"],
    )?;
    let mut fingerprints = BTreeSet::new();
    fingerprints.insert(worktree.diff_fingerprint.clone());
    for candidate in [
        worktree,
        remember_git_capture_candidate_from_repo(&RememberGitCaptureOptions {
            workspace_path: root.path(),
            mode: RememberGitCaptureMode::Commit,
            reference: Some("HEAD"),
        })
        .map_err(|error| error.message())?,
        remember_git_capture_candidate_from_repo(&RememberGitCaptureOptions {
            workspace_path: root.path(),
            mode: RememberGitCaptureMode::Diff,
            reference: Some("HEAD~1"),
        })
        .map_err(|error| error.message())?,
    ] {
        assert!(candidate.redacted);
        assert!(!candidate.content.contains(&token));
        assert!(candidate.content.contains("keep_me"));
        assert_policy_clean(&candidate);
        fingerprints.insert(candidate.diff_fingerprint);
    }
    assert_eq!(fingerprints.len(), 1);
    assert!(!root.path().join(".ee").exists());
    Ok(())
}

#[test]
fn capture_metadata_never_reintroduces_values_removed_from_the_diff() {
    let token = format!("ghp_{}", "q".repeat(36));
    let mut raw = input(RememberGitCaptureMode::Diff, "+fn keep_me() {}".to_owned());
    raw.reference = Some(format!("branch-{token}"));
    raw.changed_files = vec![format!("src/label-{token}.rs"), "src/keep.rs".to_owned()];
    let candidate = build_remember_git_capture_candidate(&raw);
    assert!(!format!("{candidate:?}").contains(&token));
    assert!(candidate.reference.is_none());
    assert_eq!(candidate.changed_files, vec!["src/keep.rs".to_owned()]);
    assert_eq!(candidate.changed_symbols, vec!["keep_me".to_owned()]);
    assert!(
        candidate
            .source
            .starts_with("git-sha://diff/redacted-reference/")
    );
    assert!(candidate.content.contains("ee-anchor:path:src/keep.rs"));
    assert!(!candidate.content.contains("ee-anchor:path:src/label"));
    assert!(
        candidate
            .redaction_reasons
            .contains(&"git_capture_reference_redacted".to_owned())
    );
    assert!(
        candidate
            .redaction_reasons
            .contains(&"git_capture_path_redacted".to_owned())
    );
    assert_policy_clean(&candidate);
}

#[test]
fn capture_withholds_sensitive_commit_ids_without_fabricating_an_object() {
    let token = format!("ghp_{}", "q".repeat(36));
    let mut raw = input(
        RememberGitCaptureMode::Commit,
        "+fn keep_me() {}".to_owned(),
    );
    // Real git returns a hexadecimal object ID. Exercise the public pure
    // transform too: its caller can supply arbitrary strings in this field.
    raw.commit_sha = Some(format!("label-{token}"));
    let candidate = build_remember_git_capture_candidate(&raw);
    assert!(!format!("{candidate:?}").contains(&token));
    assert!(candidate.commit_sha.is_none());
    assert_eq!(candidate.source, "git-sha://unknown");
    assert!(candidate.redacted);
    assert_policy_clean(&candidate);
}

#[test]
fn capture_scrubs_embedded_bearers_and_does_not_fabricate_symbol_anchors() {
    let token = format!("ghp_{}", "q".repeat(36));
    let diff =
        format!("+fn label_{token}() {{}}\n+fn keep_me() {{ let value = \"label-{token}\"; }}");
    let candidate =
        build_remember_git_capture_candidate(&input(RememberGitCaptureMode::Diff, diff));
    assert!(candidate.redacted);
    assert!(!format!("{candidate:?}").contains(&token));
    assert_eq!(candidate.changed_symbols, vec!["keep_me".to_owned()]);
    assert!(candidate.content.contains("ee-anchor:symbol:keep_me"));
    assert!(!candidate.content.contains("ee-anchor:symbol:label_"));
    assert_policy_clean(&candidate);
}

#[test]
fn capture_errors_do_not_echo_sensitive_refs_or_git_stderr() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    git(root.path(), &["init", "--initial-branch=main"])?;
    let token = format!("ghp_{}", "q".repeat(36));
    let reference = format!("missing-{token}");
    let error = git_capture_repo::commit_input(root.path(), &reference)
        .err()
        .ok_or("expected unresolved-ref failure")?;
    assert!(!error.message().contains(&token));
    assert!(!error.message().contains(&reference));
    assert!(error.message().contains("resolve capture revision"));
    assert!(!root.path().join(".ee").exists());
    Ok(())
}

#[test]
fn a_truncated_redaction_marker_never_becomes_a_symbol_anchor() {
    let token = format!("ghp_{}", "q".repeat(36));
    let prefix = "+fn ";
    // Leave only the opening bytes of the replacement marker within the
    // presentation budget. Even that fragment is not a declaration name.
    let diff = format!(
        "{prefix}{}label_{token}() {{}}",
        " ".repeat(REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES - prefix.len() - "label_[RED".len()),
    );
    let candidate =
        build_remember_git_capture_candidate(&input(RememberGitCaptureMode::Diff, diff));
    assert!(candidate.redacted);
    assert!(candidate.changed_symbols.is_empty());
    assert!(!candidate.content.contains("ee-anchor:symbol:"));
    assert!(!candidate.content.contains(&token));
}

#[test]
fn capture_preserves_ordinary_policy_names_and_real_provenance() {
    let mut raw = input(
        RememberGitCaptureMode::Commit,
        "+fn token_policy() {}".to_owned(),
    );
    raw.reference = Some("feature/password-policy".to_owned());
    raw.changed_files = vec!["src/credentials_policy.rs".to_owned()];
    let candidate = build_remember_git_capture_candidate(&raw);
    assert!(!candidate.redacted);
    assert_eq!(candidate.reference, raw.reference);
    assert_eq!(candidate.commit_sha, raw.commit_sha);
    assert_eq!(candidate.changed_files, raw.changed_files);
    assert_eq!(candidate.changed_symbols, vec!["token_policy".to_owned()]);
    assert_policy_clean(&candidate);
}
