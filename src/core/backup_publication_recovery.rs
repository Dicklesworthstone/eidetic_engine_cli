//! Recheck authority after derived rebuilding, immediately before publication.
//!
//! Rebuilding may legitimately consume jobs or change index generations, so
//! the earlier complete-inventory count fence cannot simply run again. The
//! admitted durable history, however, must remain byte-for-byte equivalent to
//! its typed recovery projection even after every rebuilding stage has run.

use std::path::Path;

use super::{HistoryExpectation, storage_error};
use super::super::RecoveryReadSnapshot;
use crate::db::{DatabaseConfig, DbConnection};
use crate::models::DomainError;

impl HistoryExpectation {
    pub(in crate::core::backup) fn verify_before_publication(
        &self,
        path: &Path,
    ) -> Result<(), DomainError> {
        let db = DbConnection::open(DatabaseConfig::read_only_file(path.to_path_buf()))
            .map_err(storage_error)?;
        let result = (|| {
            let snapshot = RecoveryReadSnapshot::begin(&db)?;
            self.verify_connection(&db)?;
            snapshot.finish()
        })();
        let closed = db.close().map(|_| ()).map_err(storage_error);
        result.and(closed)
    }
}
