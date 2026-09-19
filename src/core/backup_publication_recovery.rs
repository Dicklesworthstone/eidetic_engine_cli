//! Recheck authority after derived rebuilding, immediately before publication.
//!
//! The full recovery rebuild changes derived generations, not durable table
//! populations. Reconcile every binary-owned table obligation in the same read
//! snapshot as captured content, so extra foreign or orphaned rows cannot hide
//! behind a workspace filter after the earlier inventory fence has passed.

use std::path::Path;

use super::super::{RecoveryReadSnapshot, RestoreInventory};
use super::{HistoryExpectation, storage_error};
use crate::db::{DatabaseConfig, DbConnection};
use crate::models::DomainError;

impl HistoryExpectation {
    pub(in crate::core::backup) fn verify_before_publication(
        &self,
        path: &Path,
        inventory: &RestoreInventory,
    ) -> Result<(), DomainError> {
        let db = DbConnection::open(DatabaseConfig::read_only_file(path.to_path_buf()))
            .map_err(storage_error)?;
        let result = (|| {
            let snapshot = RecoveryReadSnapshot::begin(&db)?;
            inventory.verify_rows(&db)?;
            self.verify_connection(&db)?;
            snapshot.finish()
        })();
        let closed = db.close().map(|_| ()).map_err(storage_error);
        result.and(closed)
    }
}
