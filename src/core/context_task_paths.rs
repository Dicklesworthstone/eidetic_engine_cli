//! Literal task targets for scoped procedural guidance in context packs.
//!
//! Targets authorize scope; prose queries, tags and changed-symbol hints do
//! not. Normalize before model/index work, retain them in the request and
//! ledger, and never cache a path-dependent admission across filesystem changes.

use std::collections::BTreeSet;
use std::path::Path;
use std::str::FromStr;

use crate::models::RuleScope;
use crate::search::RuleIndexProjection;

use super::ContextPackError;

const MAX_TARGETS: usize = 64;
const MAX_TARGET_BYTES: usize = 4096;

fn invalid(reason: &str) -> ContextPackError {
    // Never quote an untrusted path (which may contain a credential or a host
    // location) in a public error. These reason codes are binary-owned.
    ContextPackError::Pack(format!(
        "Invalid pack task paths ({reason}); use at most 64 literal workspace-relative --task-path values without traversal, globs or symlink escape"
    ))
}

/// Normalize and validate the literal task paths used by pack admission.
/// This does not require the leaf to exist: new files are legitimate targets.
/// Existing ancestors still pass the rule writer's symlink-escape checks.
///
/// # Errors
/// Returns an error for excessive, nonliteral, private or escaping targets.
pub fn normalize(workspace: &Path, raw: &[String]) -> Result<Vec<String>, ContextPackError> {
    if raw.len() > MAX_TARGETS {
        return Err(invalid("too_many_targets"));
    }
    let mut targets = BTreeSet::new();
    for raw in raw {
        let value = raw.trim();
        if value.is_empty() || value.len() > MAX_TARGET_BYTES {
            return Err(invalid("invalid_target_length"));
        }
        if value.starts_with(['/', '\\', '~'])
            || value.as_bytes().get(1) == Some(&b':')
            || value.chars().any(|ch| ch.is_control() || matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
            || value.split(['/', '\\']).any(|part| part == "..")
        {
            return Err(invalid("non_literal_relative_target"));
        }
        let target = crate::search::normalize_rule_scope_pattern(
            workspace, RuleScope::FilePattern, Some(value),
        )
        .map_err(|_| invalid("unsafe_target"))?
        .ok_or_else(|| invalid("missing_target"))?;
        if crate::policy::redact_public_replay_text(&target).redacted {
            return Err(invalid("private_target"));
        }
        targets.insert(target);
    }
    Ok(targets.into_iter().collect())
}

/// The caller has already admitted the live rule's lifecycle and workspace.
/// Directory patterns match whole ancestors, not prefixes such as src-other.
/// A file-pattern rule must match a complete literal target.
pub(super) fn matches_rule(rule: &RuleIndexProjection, targets: &[String]) -> bool {
    match RuleScope::from_str(&rule.rule().scope) {
        Ok(RuleScope::Global | RuleScope::Workspace | RuleScope::Project) => true,
        Ok(scope @ (RuleScope::Directory | RuleScope::FilePattern)) => {
            let Some(pattern) = rule.normalized_scope_pattern() else { return false; };
            targets.iter().any(|path| {
                std::iter::successors(Some(path.as_str()), |&parent| {
                    (scope == RuleScope::Directory)
                        .then(|| parent.rsplit_once('/').map(|(prefix, _)| prefix))
                        .flatten()
                })
                .any(|target| crate::core::recall::recall_glob_match(pattern, target))
            })
        }
        Err(_) => false,
    }
}

/// Empty targets deliberately contribute no new bytes: historical no-target
/// requests retain their hashes. Nonempty input is normalized by the caller.
pub(super) fn hash(hasher: &mut blake3::Hasher, targets: &[String]) {
    if targets.is_empty() { return; }
    super::hash_labeled_bytes(hasher, "task_paths.schema", b"ee.pack.task_paths.v1");
    super::hash_labeled_u64(hasher, "task_paths.count", targets.len() as u64);
    for target in targets {
        super::hash_labeled_bytes(hasher, "task_paths.target", target.as_bytes());
    }
}

/// Bind a query/cursor/stream identity to its normalized task targets while
/// leaving the existing no-target wire identity unchanged.
pub fn query_hash(base: &str, targets: &[String]) -> String {
    if targets.is_empty() { return base.to_owned(); }
    let mut hasher = blake3::Hasher::new();
    super::hash_labeled_bytes(&mut hasher, "task_paths.query", base.as_bytes());
    hash(&mut hasher, targets);
    format!("blake3:{}", hasher.finalize().to_hex())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::db::{CreateProceduralRuleInput, CreateWorkspaceInput, DbConnection};

    const WORKSPACE: &str = "wsp_00000000000000000000000421";

    fn projection(root: &Path, scope: &str, pattern: Option<&str>) -> RuleIndexProjection {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        db.insert_workspace(WORKSPACE, &CreateWorkspaceInput {
            path: root.to_string_lossy().into_owned(), name: None,
        }).unwrap();
        let id = "rule_00000000000000000000000421";
        db.insert_procedural_rule(id, &CreateProceduralRuleInput {
            workspace_id: WORKSPACE.to_owned(),
            content: "Preserve the transactional outbox when updating invoice delivery.".to_owned(),
            confidence: 0.8, utility: 0.8, importance: 0.5,
            trust_class: "agent_assertion".to_owned(), scope: scope.to_owned(),
            scope_pattern: pattern.map(str::to_owned), maturity: "validated".to_owned(),
            protected: false, source_memory_ids: Vec::new(), tags: Vec::new(),
        }).unwrap();
        RuleIndexProjection::new(db.get_procedural_rule(id).unwrap().unwrap(), root, Vec::new(), Vec::new())
    }

    #[test]
    fn literal_targets_are_sorted_deduplicated_and_allow_new_files() {
        let root = tempfile::tempdir().unwrap();
        let paths = normalize(root.path(), &[
            "./src/payments/new.rs".to_owned(), "src/lib.rs".to_owned(),
            "src/payments/new.rs".to_owned(),
        ]).unwrap();
        assert_eq!(paths, ["src/lib.rs", "src/payments/new.rs"]);
        assert!(!root.path().join("src").exists(), "validation must not create paths");
    }

    #[test]
    fn ambiguous_private_and_escaping_targets_are_rejected_without_echo() {
        let root = tempfile::tempdir().unwrap();
        for bad in ["", "../outside.rs", "src/../other.rs", "/private/code.rs", "C:\\private\\code.rs", "~/code.rs", "src/*.rs", "src/[ab].rs", "src/\nfile.rs"] {
            assert!(normalize(root.path(), &[bad.to_owned()]).is_err(), "{bad:?}");
        }
        let secret = format!("src/{}{}.rs", "AKIA", "ABCDEFGHIJKLMNOP");
        let error = normalize(root.path(), std::slice::from_ref(&secret)).unwrap_err().to_string();
        assert!(!error.contains("AKIA") && !error.contains(&secret));
        assert!(normalize(root.path(), &vec!["src/lib.rs".to_owned(); 65]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn existing_symlink_ancestors_cannot_authorize_outside_files() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), root.path().join("escape")).unwrap();
        assert!(normalize(root.path(), &["escape/new.rs".to_owned()]).is_err());
    }

    #[test]
    fn directory_scope_requires_a_whole_matching_ancestor() {
        let root = tempfile::tempdir().unwrap();
        let rule = projection(root.path(), "directory", Some("src/payments"));
        assert!(rule.is_pack_admissible());
        assert!(matches_rule(&rule, &["src/payments/invoice.rs".to_owned()]));
        assert!(matches_rule(&rule, &["src/payments/nested/invoice.rs".to_owned()]));
        assert!(!matches_rule(&rule, &["src/payments-other/invoice.rs".to_owned()]));
        assert!(!matches_rule(&rule, &[]));
    }

    #[test]
    fn file_scope_matches_targets_not_task_query_or_parent_directory() {
        let root = tempfile::tempdir().unwrap();
        let rule = projection(root.path(), "file_pattern", Some("src/*.rs"));
        assert!(matches_rule(&rule, &["src/lib.rs".to_owned()]));
        assert!(!matches_rule(&rule, &["tests/lib.rs".to_owned()]));
        assert!(!matches_rule(&rule, &["src/lib.rs/notes.txt".to_owned()]));
        assert!(!matches_rule(&rule, &[]));
        let global = projection(root.path(), "workspace", None);
        assert!(matches_rule(&global, &[]));
    }

    #[test]
    fn target_hashes_are_distinct_and_do_not_change_historical_requests() {
        let root = tempfile::tempdir().unwrap();
        let a = normalize(root.path(), &["src/a.rs".into(), "src/b.rs".into()]).unwrap();
        let b = normalize(root.path(), &["./src/b.rs".into(), "src/a.rs".into(), "src/a.rs".into()]).unwrap();
        assert_eq!(query_hash("original", &[]), "original");
        assert_eq!(query_hash("original", &a), query_hash("original", &b));
        assert_ne!(query_hash("original", &a), query_hash("original", &["src/a.rs".into()]));
        assert_ne!(query_hash("other", &a), query_hash("original", &a));
        let mut old = blake3::Hasher::new(); old.update(b"original");
        let mut unchanged = old.clone(); hash(&mut unchanged, &[]);
        assert_eq!(old.finalize(), unchanged.finalize());
    }
}
