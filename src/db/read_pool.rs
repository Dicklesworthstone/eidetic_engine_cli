use std::ops::Deref;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::{DatabaseConfig, DbConnection, DbError, DbOperation, Result};

const DEFAULT_MAX_SIZE: usize = 1;
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolConfig {
    max_size: usize,
    idle_timeout: Duration,
}

impl PoolConfig {
    #[must_use]
    pub const fn new(max_size: usize, idle_timeout: Duration) -> Self {
        Self {
            max_size,
            idle_timeout,
        }
    }

    #[must_use]
    pub const fn default_single() -> Self {
        Self {
            max_size: DEFAULT_MAX_SIZE,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn requested_max_size(&self) -> usize {
        self.max_size
    }

    #[must_use]
    pub const fn max_size(&self) -> usize {
        if self.max_size == 0 { 1 } else { self.max_size }
    }

    #[must_use]
    pub const fn size_was_zero(&self) -> bool {
        self.max_size == 0
    }

    #[must_use]
    pub const fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self::default_single()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PoolStats {
    pub active: usize,
    pub idle: usize,
    pub max_size: usize,
    pub max_seen: usize,
    pub drops: u64,
    pub size_was_zero: bool,
}

pub struct ReadConnectionPool {
    database: DatabaseConfig,
    config: PoolConfig,
    state: Mutex<PoolState>,
}

struct PoolState {
    active: usize,
    idle: Vec<IdleConnection>,
    next_slot_id: u64,
    max_seen: usize,
    drops: u64,
}

struct IdleConnection {
    slot_id: u64,
    connection: DbConnection,
    returned_at: Instant,
}

pub struct PooledReadConnection<'pool> {
    pool: &'pool ReadConnectionPool,
    slot_id: u64,
    connection: Option<DbConnection>,
}

pub struct SnapshotPin<'pool> {
    connection: Option<PooledReadConnection<'pool>>,
    snapshot_active: bool,
}

impl ReadConnectionPool {
    #[must_use]
    pub fn new(database: DatabaseConfig, config: PoolConfig) -> Self {
        Self {
            database,
            config,
            state: Mutex::new(PoolState {
                active: 0,
                idle: Vec::new(),
                next_slot_id: 1,
                max_seen: 0,
                drops: 0,
            }),
        }
    }

    pub fn acquire(&self) -> Result<PooledReadConnection<'_>> {
        let max_size = self.config.max_size();
        let mut state = self.lock_state();
        let stale = evict_expired_idle(&mut state, self.config.idle_timeout());

        if let Some(idle) = state.idle.pop() {
            state.active = state.active.saturating_add(1);
            state.max_seen = state
                .max_seen
                .max(state.active.saturating_add(state.idle.len()));
            drop(state);
            drop_idle_connections(stale);
            return Ok(PooledReadConnection {
                pool: self,
                slot_id: idle.slot_id,
                connection: Some(idle.connection),
            });
        }

        if state.active.saturating_add(state.idle.len()) >= max_size {
            drop(state);
            drop_idle_connections(stale);
            return Err(DbError::MalformedRow {
                operation: DbOperation::OpenReadWrite,
                message: format!("read connection pool exhausted at max_size={max_size}"),
            });
        }

        let slot_id = state.next_slot_id;
        state.next_slot_id = state.next_slot_id.saturating_add(1);
        state.active = state.active.saturating_add(1);
        state.max_seen = state
            .max_seen
            .max(state.active.saturating_add(state.idle.len()));
        drop(state);

        drop_idle_connections(stale);
        match DbConnection::open(self.database.clone()) {
            Ok(connection) => Ok(PooledReadConnection {
                pool: self,
                slot_id,
                connection: Some(connection),
            }),
            Err(error) => {
                let mut state = self.lock_state();
                state.active = state.active.saturating_sub(1);
                Err(error)
            }
        }
    }

    pub fn pin_snapshot(&self) -> Result<SnapshotPin<'_>> {
        self.acquire_snapshot(true)
    }

    pub fn acquire_snapshot(&self, pin_snapshot: bool) -> Result<SnapshotPin<'_>> {
        let connection = self.acquire()?;
        if pin_snapshot {
            if let Err(error) = connection.begin_read_snapshot() {
                let _ = connection.rollback_read_snapshot();
                return Err(error);
            }
        }

        Ok(SnapshotPin {
            connection: Some(connection),
            snapshot_active: pin_snapshot,
        })
    }

    #[must_use]
    pub fn stats(&self) -> PoolStats {
        let state = self.lock_state();
        PoolStats {
            active: state.active,
            idle: state.idle.len(),
            max_size: self.config.max_size(),
            max_seen: state.max_seen,
            drops: state.drops,
            size_was_zero: self.config.size_was_zero(),
        }
    }

    fn release(&self, slot_id: u64, connection: DbConnection) {
        let mut to_close = None;
        {
            let mut state = self.lock_state();
            state.active = state.active.saturating_sub(1);
            if state.idle.len() < self.config.max_size() {
                state.idle.push(IdleConnection {
                    slot_id,
                    connection,
                    returned_at: Instant::now(),
                });
            } else {
                state.drops = state.drops.saturating_add(1);
                to_close = Some(connection);
            }
        }
        drop_idle_connections(to_close.into_iter().collect());
    }

    fn lock_state(&self) -> MutexGuard<'_, PoolState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl PooledReadConnection<'_> {
    #[must_use]
    pub const fn slot_id(&self) -> u64 {
        self.slot_id
    }
}

impl SnapshotPin<'_> {
    #[must_use]
    pub const fn is_pinned(&self) -> bool {
        self.snapshot_active
    }

    #[must_use]
    pub fn slot_id(&self) -> u64 {
        self.connection().slot_id()
    }

    pub fn commit(mut self) -> Result<()> {
        if self.snapshot_active {
            self.connection().commit_read_snapshot()?;
            self.snapshot_active = false;
        }
        Ok(())
    }

    fn connection(&self) -> &PooledReadConnection<'_> {
        match self.connection.as_ref() {
            Some(connection) => connection,
            None => panic!("snapshot pin owns a connection until Drop"),
        }
    }
}

impl Deref for PooledReadConnection<'_> {
    type Target = DbConnection;

    fn deref(&self) -> &Self::Target {
        match self.connection.as_ref() {
            Some(connection) => connection,
            None => panic!("pooled read connection is present until Drop"),
        }
    }
}

impl Deref for SnapshotPin<'_> {
    type Target = DbConnection;

    fn deref(&self) -> &Self::Target {
        self.connection().deref()
    }
}

impl Drop for PooledReadConnection<'_> {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            self.pool.release(self.slot_id, connection);
        }
    }
}

impl Drop for SnapshotPin<'_> {
    fn drop(&mut self) {
        if self.snapshot_active {
            if let Some(connection) = self.connection.as_ref() {
                let _ = connection.rollback_read_snapshot();
            }
            self.snapshot_active = false;
        }
    }
}

fn evict_expired_idle(state: &mut PoolState, idle_timeout: Duration) -> Vec<DbConnection> {
    if state.idle.is_empty() {
        return Vec::new();
    }

    let now = Instant::now();
    let mut retained = Vec::with_capacity(state.idle.len());
    let mut expired = Vec::new();

    for idle in state.idle.drain(..) {
        let age = now
            .checked_duration_since(idle.returned_at)
            .unwrap_or(Duration::ZERO);
        if age >= idle_timeout {
            state.drops = state.drops.saturating_add(1);
            expired.push(idle.connection);
        } else {
            retained.push(idle);
        }
    }

    state.idle = retained;
    expired
}

fn drop_idle_connections(connections: Vec<DbConnection>) {
    for connection in connections {
        let _ = connection.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn memory_pool(max_size: usize, idle_timeout: Duration) -> ReadConnectionPool {
        ReadConnectionPool::new(
            DatabaseConfig::memory(),
            PoolConfig::new(max_size, idle_timeout),
        )
    }

    fn must<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn must_some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => panic!("{context}"),
        }
    }

    fn file_pool(max_size: usize) -> (tempfile::TempDir, PathBuf, ReadConnectionPool) {
        let tempdir = must(tempfile::tempdir(), "tempdir creates");
        let database_path = tempdir.path().join("read-pool-snapshot.db");
        seed_snapshot_database(&database_path);
        let pool = ReadConnectionPool::new(
            DatabaseConfig::file(database_path.clone()),
            PoolConfig::new(max_size, Duration::from_secs(30)),
        );
        (tempdir, database_path, pool)
    }

    fn seed_snapshot_database(database_path: &Path) {
        let connection = must(
            DbConnection::open_file(database_path),
            "seed database opens",
        );
        must(
            connection
                .execute_raw("CREATE TABLE snapshot_items (id INTEGER PRIMARY KEY, value TEXT)"),
            "snapshot table creates",
        );
        must(
            connection.execute_raw("INSERT INTO snapshot_items (id, value) VALUES (1, 'before')"),
            "initial row inserts",
        );
        must(connection.close(), "seed connection closes");
    }

    fn insert_snapshot_item(database_path: &Path, id: i64, value: &str) {
        let connection = must(DbConnection::open_file(database_path), "writer opens");
        must(
            connection.execute_raw(&format!(
                "INSERT INTO snapshot_items (id, value) VALUES ({id}, '{value}')"
            )),
            "writer inserts row",
        );
        must(connection.close(), "writer closes");
    }

    fn snapshot_item_count(connection: &DbConnection) -> i64 {
        let rows = must(
            connection.query("SELECT COUNT(*) FROM snapshot_items", &[]),
            "count query succeeds",
        );
        must_some(
            rows.first()
                .and_then(|row| row.get(0).and_then(|value| value.as_i64())),
            "count row present",
        )
    }

    #[test]
    fn happy_path_pool_acquire_returns_distinct_connections_up_to_cap() {
        let pool = memory_pool(2, Duration::from_secs(30));

        let first = must(pool.acquire(), "first connection opens");
        let second = must(pool.acquire(), "second connection opens");

        assert_ne!(first.slot_id(), second.slot_id());
        assert_eq!(
            pool.stats(),
            PoolStats {
                active: 2,
                idle: 0,
                max_size: 2,
                max_seen: 2,
                drops: 0,
                size_was_zero: false,
            }
        );

        let error = match pool.acquire() {
            Ok(_) => panic!("cap should reject third acquire"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("read connection pool exhausted at max_size=2")
        );
    }

    #[test]
    fn happy_path_pool_release_returns_connection_to_lifo() {
        let pool = memory_pool(2, Duration::from_secs(30));

        let first = must(pool.acquire(), "first connection opens");
        let second = must(pool.acquire(), "second connection opens");
        let first_slot = first.slot_id();
        let second_slot = second.slot_id();
        drop(first);
        drop(second);

        assert_eq!(pool.stats().idle, 2);

        let reacquired = must(pool.acquire(), "idle connection reacquired");
        assert_eq!(reacquired.slot_id(), second_slot);
        assert_ne!(reacquired.slot_id(), first_slot);
    }

    #[test]
    fn happy_path_snapshot_pin_holds_state_across_multiple_reads() {
        let (_tempdir, database_path, pool) = file_pool(2);
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");

        assert!(pin.is_pinned());
        assert_eq!(snapshot_item_count(&pin), 1);

        insert_snapshot_item(&database_path, 2, "after");

        assert_eq!(snapshot_item_count(&pin), 1);
        drop(pin);

        let fresh = must(pool.acquire(), "fresh connection opens");
        assert_eq!(snapshot_item_count(&fresh), 2);
    }

    #[test]
    fn happy_path_two_concurrent_snapshot_pins_do_not_deadlock() {
        let (_tempdir, database_path, pool) = file_pool(2);

        let first = must(pool.pin_snapshot(), "first snapshot pin opens");
        let second = must(pool.pin_snapshot(), "second snapshot pin opens");
        assert_ne!(first.slot_id(), second.slot_id());

        assert_eq!(snapshot_item_count(&first), 1);
        assert_eq!(snapshot_item_count(&second), 1);
        insert_snapshot_item(&database_path, 2, "after");
        assert_eq!(snapshot_item_count(&first), 1);
        assert_eq!(snapshot_item_count(&second), 1);
    }

    #[test]
    fn happy_path_disabled_snapshot_pin_preserves_unpinned_read_behavior() {
        let (_tempdir, database_path, pool) = file_pool(1);
        let unpinned = must(
            pool.acquire_snapshot(false),
            "unpinned snapshot handle opens",
        );

        assert!(!unpinned.is_pinned());
        assert_eq!(snapshot_item_count(&unpinned), 1);

        insert_snapshot_item(&database_path, 2, "after");
        assert_eq!(snapshot_item_count(&unpinned), 2);
    }

    #[test]
    fn happy_path_snapshot_pin_commit_releases_pool_connection() {
        let (_tempdir, _database_path, pool) = file_pool(1);
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");

        assert_eq!(pool.stats().active, 1);
        must(pin.commit(), "read snapshot commits");

        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
    }

    #[test]
    fn empty_or_boundary_pool_size_zero_falls_back_to_size_one_with_warning() {
        let config = PoolConfig::new(0, Duration::from_secs(30));
        assert_eq!(config.requested_max_size(), 0);
        assert_eq!(config.max_size(), 1);
        assert!(config.size_was_zero());

        let pool = ReadConnectionPool::new(DatabaseConfig::memory(), config);
        let _first = must(pool.acquire(), "normalized first acquire opens");
        assert!(pool.acquire().is_err());

        let stats = pool.stats();
        assert_eq!(stats.max_size, 1);
        assert!(stats.size_was_zero);
    }

    #[test]
    fn empty_or_boundary_expired_idle_connection_is_evicted_lazily() {
        let pool = memory_pool(1, Duration::ZERO);
        assert_eq!(pool.stats().idle, 0);

        let first = must(pool.acquire(), "first connection opens");
        let first_slot = first.slot_id();
        drop(first);

        let second = must(pool.acquire(), "expired idle connection replaced");
        assert_ne!(second.slot_id(), first_slot);

        let stats = pool.stats();
        assert_eq!(stats.active, 1);
        assert_eq!(stats.idle, 0);
        assert_eq!(stats.drops, 1);
    }

    #[test]
    fn error_or_invalid_pool_acquire_when_db_is_unopenable_returns_error_not_panic() {
        let current_exe = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => panic!("current test binary path: {error}"),
        };
        let database_path = current_exe.join("read-pool.db");
        let pool = ReadConnectionPool::new(
            DatabaseConfig::file(database_path),
            PoolConfig::new(1, Duration::from_secs(30)),
        );

        let error = match pool.acquire() {
            Ok(_) => panic!("missing parent should fail"),
            Err(error) => error,
        };
        assert!(error.operation().is_some());
        assert_eq!(pool.stats().active, 0);
    }

    #[test]
    fn error_or_invalid_snapshot_pin_begin_error_returns_connection_to_pool() {
        let pool = memory_pool(1, Duration::from_secs(30));
        let connection = must(pool.acquire(), "connection opens");
        must(connection.begin(), "manual transaction begins");
        drop(connection);

        let error = match pool.pin_snapshot() {
            Ok(_) => panic!("nested snapshot begin should fail"),
            Err(error) => error,
        };
        assert!(error.operation().is_some());
        assert_eq!(pool.stats().active, 0);
        assert_eq!(pool.stats().idle, 1);

        let connection = must(pool.acquire(), "connection reacquires");
        must(
            connection.execute_raw("CREATE TABLE pin_error_reuse (id INTEGER PRIMARY KEY)"),
            "connection remains reusable after failed pin",
        );
    }
}
