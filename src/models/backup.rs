//! Backup command schemas.

/// Schema for `ee backup create` reports.
pub const BACKUP_CREATE_SCHEMA_V1: &str = "ee.backup.create.v1";

/// Schema for `ee backup list` reports.
pub const BACKUP_LIST_SCHEMA_V1: &str = "ee.backup.list.v1";

/// Schema for `ee backup verify` reports.
pub const BACKUP_VERIFY_SCHEMA_V1: &str = "ee.backup.verify.v1";

/// Schema for `ee backup inspect` reports.
pub const BACKUP_INSPECT_SCHEMA_V1: &str = "ee.backup.inspect.v1";

/// Schema for `ee backup restore` reports.
pub const BACKUP_RESTORE_SCHEMA_V1: &str = "ee.backup.restore.v1";

/// Schema for backup manifest files written by `ee backup create`.
pub const BACKUP_MANIFEST_SCHEMA_V1: &str = "ee.backup.manifest.v1";
