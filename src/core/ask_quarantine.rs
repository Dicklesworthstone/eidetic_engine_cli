//! Snapshot-local pending-review holds for answer and command-advice sources.
//!
//! Read only bound native identities, never feedback payloads or private
//! reasons. The caller owns the read snapshot and all lifecycle/scope checks.
//! A rule's source-memory lineage is not an alias for its own review status.

use std::collections::BTreeSet;

use sqlmodel_core::Value;

use crate::db::DbConnection;
use crate::models::DomainError;

const PAGE_SIZE: usize = 256;

#[derive(Clone, Copy)]
pub(super) enum Target {
    Memory,
    Rule,
}

impl Target {
    fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Rule => "rule",
        }
    }
}

fn authority_error() -> DomainError {
    DomainError::Storage {
        message: "Could not verify pending-review authority for answer sources; answer withheld"
            .to_owned(),
        repair: Some("ee doctor --json".to_owned()),
    }
}

pub(super) fn held_ids(
    connection: &DbConnection,
    workspace: &str,
    target: Target,
    ids: &[&str],
) -> Result<BTreeSet<String>, DomainError> {
    let ids = ids.iter().copied().collect::<BTreeSet<_>>();
    let ordered = ids.iter().copied().collect::<Vec<_>>();
    let mut held = BTreeSet::new();
    for page in ordered.chunks(PAGE_SIZE) {
        let mut params = vec![
            Value::Text(workspace.to_owned()),
            Value::Text(target.as_str().to_owned()),
        ];
        params.extend(page.iter().map(|id| Value::Text((*id).to_owned())));
        let slots = (3..3 + page.len())
            .map(|slot| format!("?{slot}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT DISTINCT target_id FROM feedback_quarantine WHERE workspace_id = ?1 AND target_type = ?2 AND status = 'pending' AND target_id IN ({slots}) ORDER BY target_id ASC"
        );
        for row in connection
            .query(&sql, &params)
            .map_err(|_| authority_error())?
        {
            let id = row
                .get(0)
                .and_then(Value::as_str)
                .ok_or_else(authority_error)?;
            if !ids.contains(id) {
                return Err(authority_error());
            }
            held.insert(id.to_owned());
        }
    }
    Ok(held)
}
