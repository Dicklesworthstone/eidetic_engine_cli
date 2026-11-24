//! Candidate-bounded reads for the pack contradiction guard.

use std::collections::BTreeSet;

use super::{
    DbConnection, DbOperation, Result, StoredMemoryLink, Value, stored_memory_link_from_row,
};

// Both endpoint pages share one statement; 512 parameters stay below the
// conservative SQLite bind limit even for unusually large context packs.
const ENDPOINT_PAGE_SIZE: usize = 256;

impl DbConnection {
    /// Load only conflict links whose two endpoints are selected candidates.
    /// The caller owns the snapshot shared with pack selection and resolution
    /// authority. Keep the ordinary typed decoder and graph-projection order.
    pub(crate) fn list_conflict_links_for_candidates(
        &self,
        memory_ids: &[&str],
    ) -> Result<Vec<StoredMemoryLink>> {
        let ids = memory_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut links = Vec::new();
        for sources in ids.chunks(ENDPOINT_PAGE_SIZE) {
            for destinations in ids.chunks(ENDPOINT_PAGE_SIZE) {
                let source_slots = (1..=sources.len())
                    .map(|slot| format!("?{slot}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let destination_slots = (sources.len() + 1..=sources.len() + destinations.len())
                    .map(|slot| format!("?{slot}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let params = sources
                    .iter()
                    .chain(destinations.iter())
                    .map(|id| Value::Text((*id).to_owned()))
                    .collect::<Vec<_>>();
                let sql = format!(
                    "SELECT id, src_memory_id, dst_memory_id, relation, weight, confidence, directed, evidence_count, last_reinforced_at, source, created_at, created_by, metadata_json FROM memory_links WHERE src_memory_id IN ({source_slots}) AND dst_memory_id IN ({destination_slots}) AND relation IN ('contradicts', 'supersedes') ORDER BY relation ASC, src_memory_id ASC, dst_memory_id ASC, id ASC"
                );
                for row in self.query_for(DbOperation::Query, &sql, &params)? {
                    links.push(stored_memory_link_from_row(&row)?);
                }
            }
        }
        links.sort_by(|left, right| {
            left.relation
                .cmp(&right.relation)
                .then_with(|| left.src_memory_id.cmp(&right.src_memory_id))
                .then_with(|| left.dst_memory_id.cmp(&right.dst_memory_id))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(links)
    }
}
