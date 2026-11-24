//! Effective seal authority for disclosure and derivation readers.
//!
//! A revision can inherit the seal anchored on its original memory. Keep raw
//! sidecar access separate for reveal/recovery; reader maps are keyed by the
//! requested revision while the seal retains its actual authority identity.

use super::{
    BTreeMap, BTreeSet, DbConnection, DbError, DbOperation, MemorySeal, Result, Row, Value,
    required_text, stored_memory_seal_from_row,
};

// Select an entire sidecar with CASE, not field-by-field COALESCE: NULL reveal
// fields on a directly closed seal must not inherit a root's verified reveal.
// No memory body is read by this projection.
const EFFECTIVE_MEMORY_SEAL_SELECT: &str = "SELECT \
    CASE WHEN direct_seal.memory_id IS NOT NULL THEN direct_seal.memory_id ELSE root_seal.memory_id END, \
    CASE WHEN direct_seal.memory_id IS NOT NULL THEN direct_seal.content_commitment ELSE root_seal.content_commitment END, \
    CASE WHEN direct_seal.memory_id IS NOT NULL THEN direct_seal.sealed_at ELSE root_seal.sealed_at END, \
    CASE WHEN direct_seal.memory_id IS NOT NULL THEN direct_seal.revealed_at ELSE root_seal.revealed_at END, \
    CASE WHEN direct_seal.memory_id IS NOT NULL THEN direct_seal.reveal_verified ELSE root_seal.reveal_verified END, \
    candidate.id, candidate.workspace_id, candidate.logical_id, \
    root.id, root.workspace_id, root.logical_id \
    FROM memories AS candidate \
    LEFT JOIN memories AS root ON root.id = candidate.logical_id \
    LEFT JOIN memory_seals AS direct_seal ON direct_seal.memory_id = candidate.id \
    LEFT JOIN memory_seals AS root_seal ON root_seal.memory_id = root.id";

fn invalid_seal_lineage() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Memory seal revision root could not be verified".to_owned(),
    }
}

fn nullable_authority_text(row: &Row, index: usize) -> Result<Option<&str>> {
    match row.get(index) {
        Some(Value::Null) => Ok(None),
        Some(value) => value.as_str().map(Some).ok_or_else(invalid_seal_lineage),
        None => Err(invalid_seal_lineage()),
    }
}

fn effective_memory_seal_from_row(row: &Row) -> Result<Option<(String, MemorySeal)>> {
    let candidate_id = required_text(row, 5, DbOperation::Query, "memory_id")?;
    let workspace_id = required_text(row, 6, DbOperation::Query, "workspace_id")?;
    if let Some(logical_id) = nullable_authority_text(row, 7)?
        && logical_id != candidate_id
        && (nullable_authority_text(row, 8)? != Some(logical_id)
            || nullable_authority_text(row, 9)? != Some(workspace_id)
            || nullable_authority_text(row, 10)? != Some(logical_id))
    {
        // A direct revealed sidecar cannot authenticate a broken explicit
        // lineage. NULL logical IDs on the candidate remain legacy roots.
        return Err(invalid_seal_lineage());
    }
    if nullable_authority_text(row, 0)?.is_none() {
        return Ok(None);
    }
    Ok(Some((
        candidate_id.to_owned(),
        stored_memory_seal_from_row(row)?,
    )))
}

impl DbConnection {
    /// Resolve a reader's direct or inherited seal, validating any explicit
    /// revision root before admitting content. Missing memories return `None`.
    pub fn get_effective_memory_seal(&self, memory_id: &str) -> Result<Option<MemorySeal>> {
        Ok(self
            .get_effective_memory_seals_batch(&[memory_id])?
            .remove(memory_id))
    }

    /// Resolve effective seals in bounded pages without reading memory bodies.
    /// Keys identify requested memories; values retain the actual seal's ID.
    /// Missing/unsealed candidates are absent only after a successful read.
    /// The caller owns the snapshot spanning authority and body hydration.
    pub fn get_effective_memory_seals_batch(
        &self,
        memory_ids: &[&str],
    ) -> Result<BTreeMap<String, MemorySeal>> {
        let ids: Vec<_> = memory_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut seals = BTreeMap::new();
        for page in ids.chunks(256) {
            let placeholders = (1..=page.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "{EFFECTIVE_MEMORY_SEAL_SELECT} WHERE candidate.id IN ({placeholders}) ORDER BY candidate.id ASC"
            );
            let params: Vec<_> = page
                .iter()
                .map(|id| Value::Text((*id).to_owned()))
                .collect();
            for row in self.query_for(DbOperation::Query, &sql, &params)? {
                if let Some((memory_id, seal)) = effective_memory_seal_from_row(&row)? {
                    seals.insert(memory_id, seal);
                }
            }
        }
        Ok(seals)
    }

    /// Resolve the workspace reader mask, including inherited seals on live
    /// revisions whose roots are retired. Ordinary unsealed rows are filtered
    /// in SQL; broken explicit roots remain visible to the strict decoder.
    pub fn list_effective_memory_seals(
        &self,
        workspace_id: &str,
    ) -> Result<BTreeMap<String, MemorySeal>> {
        let sql = format!(
            "{EFFECTIVE_MEMORY_SEAL_SELECT} WHERE candidate.workspace_id = ?1 AND (\
                direct_seal.memory_id IS NOT NULL OR root_seal.memory_id IS NOT NULL OR (\
                    candidate.logical_id IS NOT NULL AND candidate.logical_id != candidate.id AND (\
                        root.id IS NULL OR root.workspace_id IS NOT candidate.workspace_id OR \
                        root.logical_id IS NOT candidate.logical_id\
                    )\
                )\
            ) ORDER BY candidate.id ASC"
        );
        let mut seals = BTreeMap::new();
        for row in self.query_for(
            DbOperation::Query,
            &sql,
            &[Value::Text(workspace_id.to_owned())],
        )? {
            if let Some((memory_id, seal)) = effective_memory_seal_from_row(&row)? {
                seals.insert(memory_id, seal);
            }
        }
        Ok(seals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput, capture_query_sql_for_test};

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;
    const WORKSPACE: &str = "wsp_01234567890123456789012345";
    const FOREIGN_WORKSPACE: &str = "wsp_01234567890123456789012346";
    const STORED_AT: &str = "2026-08-08T05:00:00Z";
    const SEALED_AT: &str = "2026-08-08T06:00:00Z";
    const REVEALED_AT: &str = "2026-08-08T07:00:00Z";

    fn fixture() -> Result<DbConnection> {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        for (id, path) in [
            (WORKSPACE, "/seal/primary"),
            (FOREIGN_WORKSPACE, "/seal/foreign"),
        ] {
            connection.insert_workspace(
                id,
                &CreateWorkspaceInput {
                    path: path.to_owned(),
                    name: None,
                },
            )?;
        }
        Ok(connection)
    }

    fn memory(connection: &DbConnection, id: &str, workspace: &str, root: &str) -> Result<()> {
        connection.insert_memory_with_timestamps(
            id,
            &CreateMemoryInput {
                workspace_id: workspace.to_owned(),
                level: "semantic".to_owned(),
                kind: "fact".to_owned(),
                content: "PRIVATE_SEAL_AUTHORITY_BODY".to_owned(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.7,
                importance: 0.6,
                provenance_uri: None,
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
            STORED_AT,
            STORED_AT,
            root,
        )
    }

    fn seal(connection: &DbConnection, id: &str) -> Result<()> {
        connection.insert_memory_seal(
            id,
            &crate::models::memory_seal_commitment(b"PRIVATE_SEAL_AUTHORITY_BODY"),
            SEALED_AT,
        )
    }

    #[test]
    fn effective_seals_preserve_candidate_keys_root_identity_and_direct_null_fields() -> TestResult
    {
        let connection = fixture()?;
        let root = "mem_00000000000000000000000001";
        let inherited = "mem_00000000000000000000000002";
        let direct = "mem_00000000000000000000000003";
        let legacy = "mem_00000000000000000000000004";
        let ordinary = "mem_00000000000000000000000005";
        let foreign = "mem_00000000000000000000000006";
        for (id, workspace, logical_id) in [
            (root, WORKSPACE, root),
            (inherited, WORKSPACE, root),
            (direct, WORKSPACE, root),
            (legacy, WORKSPACE, legacy),
            (ordinary, WORKSPACE, ordinary),
            (foreign, FOREIGN_WORKSPACE, foreign),
        ] {
            memory(&connection, id, workspace, logical_id)?;
        }
        for id in [root, direct, legacy, foreign] {
            seal(&connection, id)?;
        }
        connection.execute_raw(&format!(
            "UPDATE memories SET logical_id = NULL WHERE id = '{legacy}'"
        ))?;
        assert!(connection.get_memory_seal(inherited)?.is_none());
        let closed = connection
            .get_effective_memory_seal(inherited)?
            .ok_or("missing inherited seal")?;
        assert!(closed.is_sealed());
        assert_eq!(closed.memory_id, root);
        assert!(connection.mark_memory_seal_revealed(root, REVEALED_AT)?);
        let before = connection.logical_table_digests()?;
        let mask = connection.list_effective_memory_seals(WORKSPACE)?;
        assert_eq!(mask.len(), 4);
        assert!(!mask[inherited].is_sealed());
        assert_eq!(mask[inherited].memory_id, root);
        assert!(mask[direct].is_sealed());
        assert_eq!(mask[direct].revealed_at, None);
        assert_eq!(mask[direct].reveal_verified, None);
        assert!(mask[legacy].is_sealed());
        assert!(!mask.contains_key(ordinary));
        assert!(!mask.contains_key(foreign));
        assert_eq!(connection.logical_table_digests()?, before);
        Ok(())
    }

    #[test]
    fn effective_seals_page_candidates_without_body_reads_or_n_plus_one_queries() -> TestResult {
        let connection = fixture()?;
        let ids: Vec<_> = (0..257).map(|index| format!("mem_{index:026}")).collect();
        memory(&connection, &ids[0], WORKSPACE, &ids[0])?;
        memory(&connection, &ids[1], WORKSPACE, &ids[0])?;
        memory(&connection, &ids[256], WORKSPACE, &ids[256])?;
        seal(&connection, &ids[0])?;
        seal(&connection, &ids[256])?;
        assert!(connection.mark_memory_seal_revealed(&ids[256], REVEALED_AT)?);
        let mut requested: Vec<_> = ids.iter().rev().map(String::as_str).collect();
        requested.extend([ids[0].as_str(), ids[256].as_str()]);
        let (seals, queries) =
            capture_query_sql_for_test(|| connection.get_effective_memory_seals_batch(&requested));
        let seals = seals?;
        assert_eq!(queries.len(), 2);
        assert!(queries.iter().all(|sql| {
            sql.contains("WHERE candidate.id IN (")
                && sql.contains("LEFT JOIN memory_seals AS direct_seal")
                && !sql.contains("?257")
                && !sql.contains("candidate.content")
                && !sql.contains("root.content")
        }));
        assert_eq!(seals.len(), 3);
        assert_eq!(seals[&ids[1]].memory_id, ids[0]);
        assert!(seals[&ids[1]].is_sealed());
        assert!(!seals[&ids[256]].is_sealed());
        let (point, point_queries) =
            capture_query_sql_for_test(|| connection.get_effective_memory_seal(&ids[1]));
        assert_eq!(point?, seals.get(&ids[1]).cloned());
        assert_eq!(point_queries.len(), 1);
        let (empty, empty_queries) =
            capture_query_sql_for_test(|| connection.get_effective_memory_seals_batch(&[]));
        assert!(empty?.is_empty() && empty_queries.is_empty());
        assert!(connection.get_effective_memory_seal(&ids[2])?.is_none());
        Ok(())
    }

    #[test]
    fn effective_seals_reject_broken_explicit_roots_even_with_revealed_leaf() -> TestResult {
        let connection = fixture()?;
        let valid = "mem_00000000000000000000000100";
        let missing = "mem_00000000000000000000000101";
        let foreign = "mem_00000000000000000000000102";
        let rebound = "mem_00000000000000000000000103";
        let null_root = "mem_00000000000000000000000104";
        memory(&connection, valid, WORKSPACE, valid)?;
        memory(&connection, foreign, FOREIGN_WORKSPACE, foreign)?;
        memory(&connection, rebound, WORKSPACE, valid)?;
        memory(&connection, null_root, WORKSPACE, null_root)?;
        connection.execute_raw(&format!(
            "UPDATE memories SET logical_id = NULL WHERE id = '{null_root}'"
        ))?;
        for (index, root) in [missing, foreign, rebound, null_root].iter().enumerate() {
            let child = format!("mem_{:026}", 110 + index);
            memory(&connection, &child, WORKSPACE, root)?;
            for direct_reveal in [false, true] {
                if direct_reveal {
                    seal(&connection, &child)?;
                    assert!(connection.mark_memory_seal_revealed(&child, REVEALED_AT)?);
                }
                let error = connection
                    .get_effective_memory_seal(&child)
                    .expect_err("invalid root must fail closed");
                assert!(!error.to_string().contains(root));
                assert!(!error.to_string().contains("PRIVATE_SEAL_AUTHORITY_BODY"));
                assert!(
                    connection
                        .get_effective_memory_seals_batch(&[valid, &child])
                        .is_err()
                );
                assert!(connection.list_effective_memory_seals(WORKSPACE).is_err());
                assert!(connection.get_effective_memory_seal(valid)?.is_none());
            }
        }
        assert!(
            connection
                .list_effective_memory_seals(FOREIGN_WORKSPACE)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn effective_seals_never_turn_unreadable_authority_into_absence() -> TestResult {
        let connection = fixture()?;
        let root = "mem_00000000000000000000000200";
        let child = "mem_00000000000000000000000201";
        memory(&connection, root, WORKSPACE, root)?;
        memory(&connection, child, WORKSPACE, root)?;
        seal(&connection, root)?;
        connection
            .execute_raw("UPDATE memory_seals SET sealed_at = 'PRIVATE_INVALID_AUTHORITY'")?;
        let error = connection
            .get_effective_memory_seal(child)
            .expect_err("malformed inherited authority must fail closed");
        assert!(!error.to_string().contains("PRIVATE_INVALID_AUTHORITY"));
        assert!(connection.list_effective_memory_seals(WORKSPACE).is_err());
        connection.execute_raw("ALTER TABLE memory_seals RENAME TO unavailable_effective_seals")?;
        assert!(connection.get_effective_memory_seal(child).is_err());
        assert!(connection.list_effective_memory_seals(WORKSPACE).is_err());
        let (empty, queries) =
            capture_query_sql_for_test(|| connection.get_effective_memory_seals_batch(&[]));
        assert!(empty?.is_empty() && queries.is_empty());
        Ok(())
    }
}
