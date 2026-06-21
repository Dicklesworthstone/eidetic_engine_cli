use std::collections::BTreeSet;

use serde::Serialize;

pub const GIT_AHEAD_SNAPSHOT_SCHEMA_V1: &str = "ee.git_ahead_snapshot.v1";
pub const GIT_AHEAD_DETACHED_HEAD_CODE: &str = "git_ahead_detached_head";
pub const GIT_AHEAD_LOG_FAILED_CODE: &str = "git_ahead_log_failed";
pub const GIT_AHEAD_LOG_FORMAT: &str = "%H%x1f%an%x1f%s";
pub const GIT_AHEAD_LOG_TIMEOUT_CODE: &str = "git_ahead_log_timeout";
pub const GIT_AHEAD_LOG_UNAVAILABLE_CODE: &str = "git_ahead_log_unavailable";
pub const GIT_AHEAD_LOG_COUNT_MISMATCH_CODE: &str = "git_ahead_log_count_mismatch";
pub const GIT_AHEAD_NO_UPSTREAM_CODE: &str = "git_ahead_no_upstream";
pub const GIT_AHEAD_STATUS_MISSING_AB_CODE: &str = "git_ahead_status_missing_ab";

pub const GIT_AHEAD_STATE_AMBIGUOUS: &str = "ambiguous_ahead";
pub const GIT_AHEAD_STATE_DETACHED_HEAD: &str = "detached_head";
pub const GIT_AHEAD_STATE_LOG_FAILED: &str = "git_log_failed";
pub const GIT_AHEAD_STATE_LOG_TIMEOUT: &str = "git_log_timeout";
pub const GIT_AHEAD_STATE_LOG_UNAVAILABLE: &str = "git_log_unavailable";
pub const GIT_AHEAD_STATE_MIXED_AUTHOR: &str = "mixed_author_ahead";
pub const GIT_AHEAD_STATE_MIXED_BEAD: &str = "mixed_bead_ahead";
pub const GIT_AHEAD_STATE_NO_UPSTREAM: &str = "no_upstream";
pub const GIT_AHEAD_STATE_SINGLE_OWNER: &str = "single_owner_ahead";
pub const GIT_AHEAD_STATE_STATUS_MISSING_AB: &str = "status_missing_ab";
pub const GIT_AHEAD_STATE_ZERO_AHEAD: &str = "zero_ahead";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitAheadSnapshot {
    pub schema: &'static str,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_ref: Option<String>,
    pub ahead_count: usize,
    pub behind_count: usize,
    pub commits: Vec<GitAheadCommit>,
    pub authors: Vec<String>,
    pub bead_refs: Vec<String>,
    pub mixed_author_ahead: bool,
    pub mixed_bead_ahead: bool,
    pub ambiguous_ahead: bool,
    pub peer_owned_ahead_risk: bool,
    pub degraded: Vec<GitAheadDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitAheadCommit {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub subject: String,
    pub bead_refs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitAheadDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub repair: &'static str,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct GitBranchState {
    head_ref: Option<String>,
    upstream_ref: Option<String>,
    ahead_count: Option<usize>,
    behind_count: Option<usize>,
    detached: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitAheadLogState<'a> {
    Available(&'a str),
    Unavailable,
    TimedOut,
    Failed,
}

#[must_use]
pub fn summarize_git_ahead(status_stdout: &str, log_stdout: Option<&str>) -> GitAheadSnapshot {
    let log_state = match log_stdout {
        Some(stdout) => GitAheadLogState::Available(stdout),
        None => GitAheadLogState::Unavailable,
    };
    summarize_git_ahead_with_log_state(status_stdout, log_state)
}

#[must_use]
pub fn summarize_git_ahead_with_log_state(
    status_stdout: &str,
    log_state: GitAheadLogState<'_>,
) -> GitAheadSnapshot {
    let branch = parse_git_branch_state(status_stdout);
    let mut degraded = Vec::new();

    if branch.detached {
        degraded.push(degradation(
            GIT_AHEAD_DETACHED_HEAD_CODE,
            "Git reported a detached HEAD, so upstream-ahead ownership cannot be trusted.",
            "Return to the main branch before evaluating push readiness.",
        ));
    }

    if branch.upstream_ref.is_none() {
        degraded.push(degradation(
            GIT_AHEAD_NO_UPSTREAM_CODE,
            "Git did not report an upstream branch for this checkout.",
            "Set or inspect the upstream before evaluating push readiness.",
        ));
    }

    if branch.ahead_count.is_none() || branch.behind_count.is_none() {
        degraded.push(degradation(
            GIT_AHEAD_STATUS_MISSING_AB_CODE,
            "Git status did not include branch ahead/behind counts.",
            "Run `git status --porcelain=v2 --branch` and inspect branch.ab.",
        ));
    }

    let ahead_count = branch.ahead_count.unwrap_or(0);
    let behind_count = branch.behind_count.unwrap_or(0);
    let commits = if ahead_count == 0 {
        Vec::new()
    } else {
        match log_state {
            GitAheadLogState::Available(stdout) => parse_git_ahead_log(stdout),
            GitAheadLogState::Unavailable => {
                degraded.push(degradation(
                    GIT_AHEAD_LOG_UNAVAILABLE_CODE,
                    "Git ahead commits were expected, but the git-log snapshot was unavailable.",
                    "Run `git log origin/main..HEAD --oneline --decorate` before pushing.",
                ));
                Vec::new()
            }
            GitAheadLogState::TimedOut => {
                degraded.push(degradation(
                    GIT_AHEAD_LOG_TIMEOUT_CODE,
                    "Git ahead commits were expected, but the git-log probe timed out.",
                    "Re-run the read-only git log probe with the configured timeout before pushing.",
                ));
                Vec::new()
            }
            GitAheadLogState::Failed => {
                degraded.push(degradation(
                    GIT_AHEAD_LOG_FAILED_CODE,
                    "Git ahead commits were expected, but the git-log probe failed.",
                    "Inspect the git-log failure and re-run the read-only probe before pushing.",
                ));
                Vec::new()
            }
        }
    };

    let log_count_comparable = ahead_count > 0
        && branch.upstream_ref.is_some()
        && matches!(log_state, GitAheadLogState::Available(_));
    if log_count_comparable && commits.len() != ahead_count {
        degraded.push(degradation(
            GIT_AHEAD_LOG_COUNT_MISMATCH_CODE,
            "Git ahead count did not match the parsed ahead-commit list.",
            "Re-run the read-only git status and git log probes before pushing.",
        ));
    }

    let authors = sorted_unique(commits.iter().map(|commit| commit.author.as_str()));
    let bead_refs = sorted_unique(
        commits
            .iter()
            .flat_map(|commit| commit.bead_refs.iter().map(String::as_str)),
    );
    let has_commit_without_bead = commits.iter().any(|commit| commit.bead_refs.is_empty());
    let mixed_author_ahead = authors.len() > 1;
    let mixed_bead_ahead = bead_refs.len() > 1
        || (ahead_count > 1 && !bead_refs.is_empty() && has_commit_without_bead);
    let ambiguous_bead_attribution = ahead_count > 1 && has_commit_without_bead;
    let missing_ab_status = branch.ahead_count.is_none() || branch.behind_count.is_none();
    let diverged_from_upstream = behind_count > 0;
    let ambiguous_ahead = ahead_count > 0
        && (ambiguous_bead_attribution
            || missing_ab_status
            || diverged_from_upstream
            || commits.len() != ahead_count
            || branch.upstream_ref.is_none()
            || branch.detached);
    let peer_owned_ahead_risk = ahead_count > 0
        && (mixed_author_ahead
            || mixed_bead_ahead
            || ambiguous_ahead
            || degraded
                .iter()
                .any(|entry| is_git_log_degradation(entry.code)));
    let state = ahead_state(
        &branch,
        ahead_count,
        log_state,
        mixed_author_ahead,
        mixed_bead_ahead,
        ambiguous_ahead,
    );

    GitAheadSnapshot {
        schema: GIT_AHEAD_SNAPSHOT_SCHEMA_V1,
        state,
        head_ref: branch.head_ref,
        upstream_ref: branch.upstream_ref,
        ahead_count,
        behind_count,
        commits,
        authors,
        bead_refs,
        mixed_author_ahead,
        mixed_bead_ahead,
        ambiguous_ahead,
        peer_owned_ahead_risk,
        degraded,
    }
}

#[must_use]
pub fn parse_git_ahead_log(input: &str) -> Vec<GitAheadCommit> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(parse_git_ahead_log_line)
        .collect()
}

#[must_use]
pub fn extract_bead_refs(text: &str) -> Vec<String> {
    let mut refs = BTreeSet::new();
    for (start, _) in text.match_indices("bd-") {
        let has_left_boundary = text[..start]
            .chars()
            .next_back()
            .is_none_or(is_bead_ref_boundary);
        if !has_left_boundary {
            continue;
        }
        let mut candidate = text[start..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '.')
            .collect::<String>();
        while candidate.ends_with('.') || candidate.ends_with('-') {
            candidate.pop();
        }
        let has_right_boundary = text[start + candidate.len()..]
            .chars()
            .next()
            .is_none_or(is_bead_ref_boundary);
        if !has_right_boundary {
            continue;
        }
        if candidate.len() > "bd-".len()
            && candidate["bd-".len()..]
                .chars()
                .any(|ch| ch.is_ascii_alphanumeric())
        {
            refs.insert(candidate);
        }
    }
    refs.into_iter().collect()
}

fn is_bead_ref_boundary(ch: char) -> bool {
    !ch.is_ascii_alphanumeric() && ch != '_'
}

fn parse_git_branch_state(input: &str) -> GitBranchState {
    let mut state = GitBranchState::default();
    for line in input.lines() {
        let Some(raw) = line.strip_prefix("# branch.") else {
            continue;
        };
        if let Some(value) = raw.strip_prefix("head ") {
            let value = value.trim();
            state.detached = value == "(detached)";
            state.head_ref = Some(value.to_owned());
        } else if let Some(value) = raw.strip_prefix("upstream ") {
            let value = value.trim();
            if !value.is_empty() {
                state.upstream_ref = Some(value.to_owned());
            }
        } else if let Some(value) = raw.strip_prefix("ab ") {
            let mut parts = value.split_whitespace();
            state.ahead_count = parts.next().and_then(|part| parse_signed_count(part, '+'));
            state.behind_count = parts.next().and_then(|part| parse_signed_count(part, '-'));
        }
    }
    state
}

fn parse_git_ahead_log_line(line: &str) -> Option<GitAheadCommit> {
    let mut fields = line.splitn(3, '\x1f');
    let hash = fields.next()?.trim();
    let author = fields.next()?.trim();
    let subject = fields.next()?.trim();
    if hash.is_empty() || author.is_empty() || subject.is_empty() {
        return None;
    }
    Some(GitAheadCommit {
        hash: hash.to_owned(),
        short_hash: hash.chars().take(12).collect(),
        author: author.to_owned(),
        subject: subject.to_owned(),
        bead_refs: extract_bead_refs(subject),
    })
}

fn is_git_log_degradation(code: &str) -> bool {
    code == GIT_AHEAD_LOG_UNAVAILABLE_CODE
        || code == GIT_AHEAD_LOG_TIMEOUT_CODE
        || code == GIT_AHEAD_LOG_FAILED_CODE
        || code == GIT_AHEAD_LOG_COUNT_MISMATCH_CODE
}

fn ahead_state(
    branch: &GitBranchState,
    ahead_count: usize,
    log_state: GitAheadLogState<'_>,
    mixed_author_ahead: bool,
    mixed_bead_ahead: bool,
    ambiguous_ahead: bool,
) -> &'static str {
    if branch.detached {
        return GIT_AHEAD_STATE_DETACHED_HEAD;
    }
    if branch.upstream_ref.is_none() {
        return GIT_AHEAD_STATE_NO_UPSTREAM;
    }
    if branch.ahead_count.is_none() || branch.behind_count.is_none() {
        return GIT_AHEAD_STATE_STATUS_MISSING_AB;
    }
    if ahead_count == 0 {
        return GIT_AHEAD_STATE_ZERO_AHEAD;
    }
    match log_state {
        GitAheadLogState::Available(_) => {}
        GitAheadLogState::Unavailable => return GIT_AHEAD_STATE_LOG_UNAVAILABLE,
        GitAheadLogState::TimedOut => return GIT_AHEAD_STATE_LOG_TIMEOUT,
        GitAheadLogState::Failed => return GIT_AHEAD_STATE_LOG_FAILED,
    }
    if mixed_author_ahead {
        GIT_AHEAD_STATE_MIXED_AUTHOR
    } else if mixed_bead_ahead {
        GIT_AHEAD_STATE_MIXED_BEAD
    } else if ambiguous_ahead {
        GIT_AHEAD_STATE_AMBIGUOUS
    } else {
        GIT_AHEAD_STATE_SINGLE_OWNER
    }
}

fn parse_signed_count(raw: &str, sign: char) -> Option<usize> {
    raw.strip_prefix(sign)?.parse().ok()
}

fn sorted_unique<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    values
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn degradation(
    code: &'static str,
    message: &'static str,
    repair: &'static str,
) -> GitAheadDegradation {
    GitAheadDegradation {
        code,
        severity: "warning",
        message,
        repair,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean_status(ahead: usize) -> String {
        format!(
            "# branch.oid abcdef\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +{ahead} -0\n"
        )
    }

    #[test]
    fn zero_ahead_snapshot_is_not_risky() {
        let snapshot = summarize_git_ahead(&clean_status(0), Some(""));

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_ZERO_AHEAD);
        assert_eq!(snapshot.upstream_ref.as_deref(), Some("origin/main"));
        assert_eq!(snapshot.ahead_count, 0);
        assert!(snapshot.commits.is_empty());
        assert!(!snapshot.peer_owned_ahead_risk);
        assert!(snapshot.degraded.is_empty());
    }

    #[test]
    fn single_owner_ahead_snapshot_keeps_compact_commit_metadata() {
        let snapshot = summarize_git_ahead(
            &clean_status(1),
            Some(
                "730f16a6abcdef\x1fCodex\x1ftest(repair): gate fallback safety metadata (bd-3g4r4.5)\n",
            ),
        );

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_SINGLE_OWNER);
        assert_eq!(snapshot.ahead_count, 1);
        assert_eq!(snapshot.authors, vec!["Codex"]);
        assert_eq!(snapshot.bead_refs, vec!["bd-3g4r4.5"]);
        assert!(!snapshot.mixed_author_ahead);
        assert!(!snapshot.mixed_bead_ahead);
        assert!(!snapshot.ambiguous_ahead);
        assert!(!snapshot.peer_owned_ahead_risk);
        assert_eq!(snapshot.commits[0].short_hash, "730f16a6abcd");
    }

    #[test]
    fn single_owner_ahead_without_bead_ref_stays_non_risky() {
        let snapshot = summarize_git_ahead(
            &clean_status(1),
            Some(
                "730f16a6abcdef\x1fCodex\x1fchore(beads): file review findings from R1 cod_4 pass\n",
            ),
        );

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_SINGLE_OWNER);
        assert_eq!(snapshot.ahead_count, 1);
        assert_eq!(snapshot.authors, vec!["Codex"]);
        assert!(snapshot.bead_refs.is_empty());
        assert!(!snapshot.mixed_author_ahead);
        assert!(!snapshot.mixed_bead_ahead);
        assert!(!snapshot.ambiguous_ahead);
        assert!(!snapshot.peer_owned_ahead_risk);
    }

    #[test]
    fn diverged_ahead_and_behind_snapshot_is_risky() {
        let snapshot = summarize_git_ahead(
            concat!(
                "# branch.oid abcdef\n",
                "# branch.head main\n",
                "# branch.upstream origin/main\n",
                "# branch.ab +1 -1\n",
            ),
            Some("730f16a6abcdef\x1fCodex\x1ffix: compact metadata (bd-2gc7r.1)\n"),
        );

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_AMBIGUOUS);
        assert_eq!(snapshot.ahead_count, 1);
        assert_eq!(snapshot.behind_count, 1);
        assert!(!snapshot.mixed_author_ahead);
        assert!(!snapshot.mixed_bead_ahead);
        assert!(snapshot.ambiguous_ahead);
        assert!(snapshot.peer_owned_ahead_risk);
        assert!(snapshot.degraded.is_empty());
    }

    #[test]
    fn mixed_author_ahead_snapshot_is_risky() {
        let snapshot = summarize_git_ahead(
            &clean_status(2),
            Some(concat!(
                "1111111111111111\x1fCodex\x1ffix: parser (bd-2gc7r.1)\n",
                "2222222222222222\x1fPeerAgent\x1ftest: fixture (bd-peer.7)\n",
            )),
        );

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_MIXED_AUTHOR);
        assert!(snapshot.mixed_author_ahead);
        assert!(snapshot.mixed_bead_ahead);
        assert!(snapshot.peer_owned_ahead_risk);
        assert_eq!(snapshot.authors, vec!["Codex", "PeerAgent"]);
    }

    #[test]
    fn mixed_bead_or_missing_bead_snapshot_is_ambiguous() {
        let snapshot = summarize_git_ahead(
            &clean_status(2),
            Some(concat!(
                "1111111111111111\x1fCodex\x1ffix: parser (bd-2gc7r.1)\n",
                "2222222222222222\x1fCodex\x1fdocs: unrelated follow-up\n",
            )),
        );

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_MIXED_BEAD);
        assert!(!snapshot.mixed_author_ahead);
        assert!(snapshot.mixed_bead_ahead);
        assert!(snapshot.ambiguous_ahead);
        assert!(snapshot.peer_owned_ahead_risk);
    }

    #[test]
    fn no_upstream_degrades_without_claiming_safe_push_readiness() {
        let snapshot = summarize_git_ahead(
            "# branch.oid abcdef\n# branch.head main\n# branch.ab +0 -0\n",
            Some(""),
        );

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_NO_UPSTREAM);
        assert!(snapshot.upstream_ref.is_none());
        assert!(
            snapshot
                .degraded
                .iter()
                .any(|entry| entry.code == GIT_AHEAD_NO_UPSTREAM_CODE)
        );
    }

    #[test]
    fn log_unavailable_for_positive_ahead_is_risky() {
        let snapshot = summarize_git_ahead(&clean_status(2), None);

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_LOG_UNAVAILABLE);
        assert_eq!(snapshot.ahead_count, 2);
        assert!(snapshot.commits.is_empty());
        assert!(snapshot.peer_owned_ahead_risk);
        assert!(
            snapshot
                .degraded
                .iter()
                .any(|entry| entry.code == GIT_AHEAD_LOG_UNAVAILABLE_CODE)
        );
        assert!(
            snapshot
                .degraded
                .iter()
                .all(|entry| entry.code != GIT_AHEAD_LOG_COUNT_MISMATCH_CODE)
        );
    }

    #[test]
    fn log_timeout_for_positive_ahead_is_distinct_from_failure() {
        let snapshot =
            summarize_git_ahead_with_log_state(&clean_status(1), GitAheadLogState::TimedOut);

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_LOG_TIMEOUT);
        assert!(snapshot.peer_owned_ahead_risk);
        assert!(
            snapshot
                .degraded
                .iter()
                .any(|entry| entry.code == GIT_AHEAD_LOG_TIMEOUT_CODE)
        );
        assert!(
            snapshot
                .degraded
                .iter()
                .all(|entry| entry.code != GIT_AHEAD_LOG_FAILED_CODE)
        );
        assert!(
            snapshot
                .degraded
                .iter()
                .all(|entry| entry.code != GIT_AHEAD_LOG_COUNT_MISMATCH_CODE)
        );
    }

    #[test]
    fn log_failure_for_positive_ahead_is_distinct_from_timeout() {
        let snapshot =
            summarize_git_ahead_with_log_state(&clean_status(1), GitAheadLogState::Failed);

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_LOG_FAILED);
        assert!(snapshot.peer_owned_ahead_risk);
        assert!(
            snapshot
                .degraded
                .iter()
                .any(|entry| entry.code == GIT_AHEAD_LOG_FAILED_CODE)
        );
        assert!(
            snapshot
                .degraded
                .iter()
                .all(|entry| entry.code != GIT_AHEAD_LOG_TIMEOUT_CODE)
        );
        assert!(
            snapshot
                .degraded
                .iter()
                .all(|entry| entry.code != GIT_AHEAD_LOG_COUNT_MISMATCH_CODE)
        );
    }

    #[test]
    fn available_log_count_mismatch_still_degrades() {
        let snapshot = summarize_git_ahead(
            &clean_status(2),
            Some("1111111111111111\x1fCodex\x1ffix: parser (bd-2gc7r.1)\n"),
        );

        assert!(snapshot.ambiguous_ahead);
        assert!(snapshot.peer_owned_ahead_risk);
        assert!(
            snapshot
                .degraded
                .iter()
                .any(|entry| entry.code == GIT_AHEAD_LOG_COUNT_MISMATCH_CODE)
        );
    }

    #[test]
    fn empty_author_ahead_log_row_is_not_reported_safe() {
        let snapshot = summarize_git_ahead(
            &clean_status(1),
            Some("1111111111111111\x1f\x1ffix: unknown owner (bd-2gc7r.1)\n"),
        );

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_AMBIGUOUS);
        assert!(snapshot.commits.is_empty());
        assert!(snapshot.ambiguous_ahead);
        assert!(snapshot.peer_owned_ahead_risk);
        assert!(
            snapshot
                .degraded
                .iter()
                .any(|entry| entry.code == GIT_AHEAD_LOG_COUNT_MISMATCH_CODE)
        );
    }

    #[test]
    fn detached_head_gets_deterministic_state_even_with_ahead_data() {
        let snapshot = summarize_git_ahead(
            "# branch.oid abcdef\n# branch.head (detached)\n# branch.upstream origin/main\n# branch.ab +1 -0\n",
            Some("1111111111111111\x1fCodex\x1ffix: detached case (bd-2gc7r.1)\n"),
        );

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_DETACHED_HEAD);
        assert!(snapshot.ambiguous_ahead);
        assert!(snapshot.peer_owned_ahead_risk);
        assert!(
            snapshot
                .degraded
                .iter()
                .any(|entry| entry.code == GIT_AHEAD_DETACHED_HEAD_CODE)
        );
    }

    #[test]
    fn partial_branch_ab_with_positive_ahead_stays_risky() {
        let snapshot = summarize_git_ahead(
            concat!(
                "# branch.oid abcdef\n",
                "# branch.head main\n",
                "# branch.upstream origin/main\n",
                "# branch.ab +1 missing-behind\n",
            ),
            Some("1111111111111111\x1fCodex\x1ffix: partial branch status (bd-2gc7r.1)\n"),
        );

        assert_eq!(snapshot.state, GIT_AHEAD_STATE_STATUS_MISSING_AB);
        assert_eq!(snapshot.ahead_count, 1);
        assert!(snapshot.ambiguous_ahead);
        assert!(snapshot.peer_owned_ahead_risk);
        assert!(
            snapshot
                .degraded
                .iter()
                .any(|entry| entry.code == GIT_AHEAD_STATUS_MISSING_AB_CODE)
        );
    }

    #[test]
    fn bead_ref_extractor_handles_br_prefixes_and_punctuation() {
        assert_eq!(
            extract_bead_refs("commit [br-bd-f6jfs.8], follow-up bd-2gc7r.1."),
            vec!["bd-2gc7r.1", "bd-f6jfs.8"]
        );
    }

    #[test]
    fn bead_ref_extractor_rejects_embedded_word_fragments() {
        assert_eq!(
            extract_bead_refs(
                "notbd-123, abcbd-456, foo_bd-111, and bd-222_suffix are not refs; (bd-789) is"
            ),
            vec!["bd-789"]
        );
    }

    #[test]
    fn serialized_snapshot_uses_camel_case_contract_fields() {
        let snapshot = summarize_git_ahead(
            &clean_status(1),
            Some("730f16a6abcdef\x1fCodex\x1ffix: compact metadata (bd-2gc7r.1)\n"),
        );
        let rendered = serde_json::to_value(snapshot).expect("snapshot serializes");

        assert_eq!(rendered["schema"], GIT_AHEAD_SNAPSHOT_SCHEMA_V1);
        assert_eq!(rendered["state"], GIT_AHEAD_STATE_SINGLE_OWNER);
        assert!(rendered.get("aheadCount").is_some());
        assert!(rendered.get("peerOwnedAheadRisk").is_some());
        assert!(rendered["commits"][0].get("authorEmail").is_none());
        assert!(rendered.get("ahead_count").is_none());
    }
}
