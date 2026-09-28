//! Exact decision ancestry from native identities and directed supersedes edges.
//!
//! This is source-lineage validation, not a graph ranking metric. All reads
//! borrow the caller's snapshot. A cache belongs to that one snapshot only.
//! Ancestor bodies, typed fields, review reasons and link annotations are never
//! loaded merely to count history; withheld ancestors remain lineage identities.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use sqlmodel_core::Value;

use crate::db::DbConnection;
use crate::models::{DomainError, MemoryId};

const PAGE_SIZE: usize = 256;

fn lineage_error() -> DomainError {
    super::decide_storage_error(
        "Could not verify a complete workspace-owned decision lineage; decision withheld",
    )
}

pub(super) fn successor_depth(depth: u32) -> Result<u32, DomainError> {
    depth.checked_add(1).ok_or_else(lineage_error)
}

pub(super) fn chain_depth(
    connection: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
) -> Result<u32, DomainError> {
    let mut lineage = DecisionLineage::new(workspace_id);
    lineage.load(connection, &[memory_id])?;
    lineage.depth(memory_id)
}

/// Memoize complete chains across the source pages of one decision operation.
/// Do not share this cache between requests or across a transaction boundary.
pub(super) struct DecisionLineage {
    workspace_id: String,
    predecessors: BTreeMap<String, Option<String>>,
    depths: BTreeMap<String, u32>,
}

impl DecisionLineage {
    pub(super) fn new(workspace_id: &str) -> Self {
        Self {
            workspace_id: workspace_id.to_owned(),
            predecessors: BTreeMap::new(),
            depths: BTreeMap::new(),
        }
    }

    pub(super) fn depth(&self, memory_id: &str) -> Result<u32, DomainError> {
        self.depths
            .get(memory_id)
            .copied()
            .ok_or_else(lineage_error)
    }

    pub(super) fn load(
        &mut self,
        connection: &DbConnection,
        roots: &[&str],
    ) -> Result<(), DomainError> {
        self.load_with_observer(connection, roots, |_| Ok(()))
    }

    // Deterministic real-store tests can commit a concurrent edge between
    // frontier pages. Production passes a no-op; no global hook or extra IO.
    fn load_with_observer(
        &mut self,
        connection: &DbConnection,
        roots: &[&str],
        mut after_page: impl FnMut(&[String]) -> Result<(), DomainError>,
    ) -> Result<(), DomainError> {
        let mut pending: BTreeSet<_> = roots
            .iter()
            .filter(|id| !self.predecessors.contains_key(**id))
            .map(|id| (*id).to_owned())
            .collect();
        while !pending.is_empty() {
            let mut page = Vec::with_capacity(PAGE_SIZE);
            while page.len() < PAGE_SIZE {
                let Some(id) = pending.pop_first() else {
                    break;
                };
                if !canonical_id(&id) {
                    return Err(lineage_error());
                }
                page.push(id);
            }
            let predecessors = self.read_page(connection, &page)?;
            for parent in predecessors.values().flatten() {
                if !self.predecessors.contains_key(parent) && !predecessors.contains_key(parent) {
                    pending.insert(parent.clone());
                }
            }
            self.predecessors.extend(predecessors);
            after_page(&page)?;
        }
        // No recursive stack, arbitrary 64-link cutoff, or first-edge choice.
        // Each completed shared tail is reused; an active path revisiting one
        // of its own identities is a cycle, never a successful short history.
        for root in roots {
            let mut path = Vec::new();
            let mut active = BTreeSet::new();
            let mut current = (*root).to_owned();
            let mut depth = loop {
                if let Some(depth) = self.depths.get(&current) {
                    break *depth;
                }
                if !active.insert(current.clone()) {
                    return Err(lineage_error());
                }
                match self.predecessors.get(&current).ok_or_else(lineage_error)? {
                    None => {
                        self.depths.insert(current, 0);
                        break 0;
                    }
                    Some(parent) => {
                        path.push(current);
                        current = parent.clone();
                    }
                }
            };
            for id in path.into_iter().rev() {
                depth = successor_depth(depth)?;
                self.depths.insert(id, depth);
            }
        }
        Ok(())
    }

    fn read_page(
        &self,
        connection: &DbConnection,
        page: &[String],
    ) -> Result<BTreeMap<String, Option<String>>, DomainError> {
        let slots = (1..=page.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let params: Vec<_> = page.iter().map(|id| Value::Text(id.clone())).collect();
        let rows = connection
            .query(
                &format!(
                    "SELECT id, workspace_id, kind FROM memories WHERE id IN ({slots}) ORDER BY id ASC"
                ),
                &params,
            )
            .map_err(|_| lineage_error())?;
        if rows.len() != page.len() {
            return Err(lineage_error());
        }
        let mut predecessors = BTreeMap::new();
        for row in rows {
            let id = row
                .get(0)
                .and_then(Value::as_str)
                .ok_or_else(lineage_error)?;
            if page
                .binary_search_by(|candidate| candidate.as_str().cmp(id))
                .is_err()
                || row.get(1).and_then(Value::as_str) != Some(self.workspace_id.as_str())
                || row.get(2).and_then(Value::as_str) != Some("decision")
                || predecessors.insert(id.to_owned(), None).is_some()
            {
                return Err(lineage_error());
            }
        }
        // At most one outgoing predecessor per decision is a valid chain.
        // Bound a corrupt fan-out's result allocation to page_size + 1; seeing
        // the extra row is already proof that a complete valid page is absent.
        let sql = format!(
            "SELECT src_memory_id, dst_memory_id FROM memory_links WHERE relation = 'supersedes' AND directed = 1 AND src_memory_id IN ({slots}) ORDER BY src_memory_id ASC, dst_memory_id ASC LIMIT ?{}",
            page.len() + 1,
        );
        let mut params = params;
        params.push(Value::BigInt((page.len() + 1) as i64));
        let links = connection
            .query(&sql, &params)
            .map_err(|_| lineage_error())?;
        if links.len() > page.len() {
            return Err(lineage_error());
        }
        for row in links {
            let source = row
                .get(0)
                .and_then(Value::as_str)
                .ok_or_else(lineage_error)?;
            let parent = row
                .get(1)
                .and_then(Value::as_str)
                .ok_or_else(lineage_error)?;
            if !canonical_id(parent) {
                return Err(lineage_error());
            }
            let previous = predecessors.get_mut(source).ok_or_else(lineage_error)?;
            if previous.replace(parent.to_owned()).is_some() {
                return Err(lineage_error());
            }
        }
        Ok(predecessors)
    }
}

fn canonical_id(id: &str) -> bool {
    MemoryId::from_str(id).is_ok_and(|parsed| parsed.to_string() == id)
}

#[cfg(test)]
#[path = "decide_lineage_tests.rs"]
mod tests;
