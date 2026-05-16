use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::{DatabaseConfig, DbConnection, DbError, DbOperation, Result};

const DEFAULT_MAX_SIZE: usize = 1;
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_PIN_DURATION: Duration = Duration::from_secs(30);
const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const ACQUIRE_WAIT_SAMPLE_CAP: usize = 1024;
const ACQUIRE_SLEEP_STEP: Duration = Duration::from_millis(1);

pub const SNAPSHOT_PIN_EXPIRED_CODE: &str = "snapshot_pin_expired";
pub const SNAPSHOT_RELEASE_FAILED_CODE: &str = "snapshot_release_failed";
pub const SNAPSHOT_PIN_FORCE_RELEASED_CODE: &str = "snapshot_pin_force_released";
pub const READ_POOL_ACQUIRE_TIMEOUT_CODE: &str = "read_pool_acquire_timeout";
pub const READ_POOL_UNDERSIZED_CODE: &str = "read_pool_undersized";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolConfig {
    max_size: usize,
    idle_timeout: Duration,
    max_pin_duration: Duration,
    acquire_timeout: Duration,
}

impl PoolConfig {
    #[must_use]
    pub const fn new(max_size: usize, idle_timeout: Duration) -> Self {
        Self {
            max_size,
            idle_timeout,
            max_pin_duration: DEFAULT_MAX_PIN_DURATION,
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn default_single() -> Self {
        Self {
            max_size: DEFAULT_MAX_SIZE,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_pin_duration: DEFAULT_MAX_PIN_DURATION,
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn with_max_pin_duration(mut self, max_pin_duration: Duration) -> Self {
        self.max_pin_duration = max_pin_duration;
        self
    }

    #[must_use]
    pub const fn with_acquire_timeout(mut self, acquire_timeout: Duration) -> Self {
        self.acquire_timeout = acquire_timeout;
        self
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

    #[must_use]
    pub const fn max_pin_duration(&self) -> Duration {
        self.max_pin_duration
    }

    #[must_use]
    pub const fn acquire_timeout(&self) -> Duration {
        self.acquire_timeout
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
    pub active_pins: usize,
    pub expired_pins: usize,
    pub max_size: usize,
    pub max_seen: usize,
    pub drops: u64,
    pub ad_hoc_bypass_count: u64,
    pub acquire_wait: AcquireWaitStats,
    pub size_was_zero: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AcquireWaitStats {
    pub samples: usize,
    pub p50_ns: u128,
    pub p99_ns: u128,
}

pub struct ReadConnectionPool {
    database: DatabaseConfig,
    config: PoolConfig,
    state: Mutex<PoolState>,
}

struct PoolState {
    active: usize,
    idle: Vec<IdleConnection>,
    acquire_wait_ns: Vec<u128>,
    next_slot_id: u64,
    next_pin_id: u64,
    active_pins: BTreeMap<u64, ActivePinRecord>,
    max_seen: usize,
    drops: u64,
    ad_hoc_bypass_count: u64,
}

struct ActivePinRecord {
    slot_id: Option<u64>,
    acquired_at: Instant,
    max_pin_duration: Duration,
    poisoned: Arc<AtomicBool>,
}

struct IdleConnection {
    slot_id: u64,
    connection: DbConnection,
    returned_at: Instant,
}

pub struct PooledReadConnection<'pool> {
    pool: Option<&'pool ReadConnectionPool>,
    slot_id: Option<u64>,
    connection: Option<DbConnection>,
}

pub struct SnapshotPin<'pool> {
    connection: Option<PooledReadConnection<'pool>>,
    snapshot_active: bool,
    pin_id: Option<u64>,
    poisoned: Arc<AtomicBool>,
    acquired_at: Instant,
    max_pin_duration: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpiredSnapshotPin {
    pub pin_id: u64,
    pub slot_id: Option<u64>,
    pub age: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotDrainReport {
    pub drained: bool,
    pub waited: Duration,
    pub active_pins_remaining: usize,
    pub force_poisoned: Vec<ExpiredSnapshotPin>,
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
                acquire_wait_ns: Vec::with_capacity(ACQUIRE_WAIT_SAMPLE_CAP),
                next_slot_id: 1,
                next_pin_id: 1,
                active_pins: BTreeMap::new(),
                max_seen: 0,
                drops: 0,
                ad_hoc_bypass_count: 0,
            }),
        }
    }

    pub fn acquire(&self) -> Result<PooledReadConnection<'_>> {
        let max_size = self.config.max_size();
        let started = Instant::now();

        loop {
            let mut state = self.lock_state();
            let stale = evict_expired_idle(&mut state, self.config.idle_timeout());

            if let Some(idle) = state.idle.pop() {
                state.active = state.active.saturating_add(1);
                state.max_seen = state
                    .max_seen
                    .max(state.active.saturating_add(state.idle.len()));
                record_acquire_wait(&mut state, started.elapsed());
                drop(state);
                drop_idle_connections(stale);
                return Ok(PooledReadConnection {
                    pool: Some(self),
                    slot_id: Some(idle.slot_id),
                    connection: Some(idle.connection),
                });
            }

            if state.active.saturating_add(state.idle.len()) < max_size {
                let slot_id = state.next_slot_id;
                state.next_slot_id = state.next_slot_id.saturating_add(1);
                state.active = state.active.saturating_add(1);
                state.max_seen = state
                    .max_seen
                    .max(state.active.saturating_add(state.idle.len()));
                record_acquire_wait(&mut state, started.elapsed());
                drop(state);

                drop_idle_connections(stale);
                return match DbConnection::open(self.database.clone()) {
                    Ok(connection) => Ok(PooledReadConnection {
                        pool: Some(self),
                        slot_id: Some(slot_id),
                        connection: Some(connection),
                    }),
                    Err(error) => {
                        let mut state = self.lock_state();
                        state.active = state.active.saturating_sub(1);
                        Err(error)
                    }
                };
            }

            let elapsed = started.elapsed();
            if elapsed >= self.config.acquire_timeout() {
                state.ad_hoc_bypass_count = state.ad_hoc_bypass_count.saturating_add(1);
                record_acquire_wait(&mut state, elapsed);
                drop(state);
                drop_idle_connections(stale);
                return DbConnection::open(self.database.clone()).map(|connection| {
                    PooledReadConnection {
                        pool: None,
                        slot_id: None,
                        connection: Some(connection),
                    }
                });
            }

            drop(state);
            drop_idle_connections(stale);
            let remaining = self.config.acquire_timeout().saturating_sub(elapsed);
            std::thread::yield_now();
            std::thread::sleep(remaining.min(ACQUIRE_SLEEP_STEP));
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

        let acquired_at = Instant::now();
        let (pin_id, poisoned) = if pin_snapshot {
            let (pin_id, poisoned) = self.register_pin(
                connection.slot_id(),
                acquired_at,
                self.config.max_pin_duration(),
            );
            (Some(pin_id), poisoned)
        } else {
            (None, Arc::new(AtomicBool::new(false)))
        };

        Ok(SnapshotPin {
            connection: Some(connection),
            snapshot_active: pin_snapshot,
            pin_id,
            poisoned,
            acquired_at,
            max_pin_duration: self.config.max_pin_duration(),
        })
    }

    #[must_use]
    pub fn stats(&self) -> PoolStats {
        let state = self.lock_state();
        PoolStats {
            active: state.active,
            idle: state.idle.len(),
            active_pins: state.active_pins.len(),
            expired_pins: expired_pin_count(&state.active_pins),
            max_size: self.config.max_size(),
            max_seen: state.max_seen,
            drops: state.drops,
            ad_hoc_bypass_count: state.ad_hoc_bypass_count,
            acquire_wait: acquire_wait_stats(&state.acquire_wait_ns),
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

    fn abandon(&self, connection: DbConnection) {
        {
            let mut state = self.lock_state();
            state.active = state.active.saturating_sub(1);
            state.drops = state.drops.saturating_add(1);
        }
        let _ = connection.close();
    }

    pub fn expire_stale_pins(&self) -> Vec<ExpiredSnapshotPin> {
        let now = Instant::now();
        let state = self.lock_state();
        let mut expired = Vec::new();

        for (pin_id, record) in &state.active_pins {
            let age = now
                .checked_duration_since(record.acquired_at)
                .unwrap_or(Duration::ZERO);
            if age >= record.max_pin_duration && !record.poisoned.swap(true, Ordering::AcqRel) {
                expired.push(ExpiredSnapshotPin {
                    pin_id: *pin_id,
                    slot_id: record.slot_id,
                    age,
                });
            }
        }

        expired
    }

    pub fn force_poison_active_pins(&self) -> Vec<ExpiredSnapshotPin> {
        let now = Instant::now();
        let state = self.lock_state();
        let mut poisoned = Vec::new();

        for (pin_id, record) in &state.active_pins {
            record.poisoned.store(true, Ordering::Release);
            poisoned.push(ExpiredSnapshotPin {
                pin_id: *pin_id,
                slot_id: record.slot_id,
                age: now
                    .checked_duration_since(record.acquired_at)
                    .unwrap_or(Duration::ZERO),
            });
        }

        poisoned
    }

    pub fn drain_snapshot_pins(&self, timeout: Duration) -> SnapshotDrainReport {
        let started = Instant::now();

        loop {
            let active_pins = self.lock_state().active_pins.len();
            if active_pins == 0 {
                return SnapshotDrainReport {
                    drained: true,
                    waited: started.elapsed(),
                    active_pins_remaining: 0,
                    force_poisoned: Vec::new(),
                };
            }

            let elapsed = started.elapsed();
            if elapsed >= timeout {
                let force_poisoned = self.force_poison_active_pins();
                return SnapshotDrainReport {
                    drained: false,
                    waited: elapsed,
                    active_pins_remaining: self.lock_state().active_pins.len(),
                    force_poisoned,
                };
            }

            std::thread::yield_now();
            std::thread::sleep(timeout.saturating_sub(elapsed).min(ACQUIRE_SLEEP_STEP));
        }
    }

    fn register_pin(
        &self,
        slot_id: Option<u64>,
        acquired_at: Instant,
        max_pin_duration: Duration,
    ) -> (u64, Arc<AtomicBool>) {
        let mut state = self.lock_state();
        let pin_id = state.next_pin_id;
        state.next_pin_id = state.next_pin_id.saturating_add(1);
        let poisoned = Arc::new(AtomicBool::new(false));
        state.active_pins.insert(
            pin_id,
            ActivePinRecord {
                slot_id,
                acquired_at,
                max_pin_duration,
                poisoned: Arc::clone(&poisoned),
            },
        );
        (pin_id, poisoned)
    }

    fn unregister_pin(&self, pin_id: u64) {
        let mut state = self.lock_state();
        state.active_pins.remove(&pin_id);
    }

    fn lock_state(&self) -> MutexGuard<'_, PoolState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl PooledReadConnection<'_> {
    #[must_use]
    pub const fn slot_id(&self) -> Option<u64> {
        self.slot_id
    }

    #[must_use]
    pub const fn is_ad_hoc(&self) -> bool {
        self.pool.is_none()
    }

    fn abandon(&mut self) {
        if let Some(connection) = self.connection.take() {
            if let Some(pool) = self.pool {
                pool.abandon(connection);
            } else {
                let _ = connection.close();
            }
        }
    }
}

impl SnapshotPin<'_> {
    #[must_use]
    pub const fn is_pinned(&self) -> bool {
        self.snapshot_active
    }

    #[must_use]
    pub fn slot_id(&self) -> Option<u64> {
        self.connection().slot_id()
    }

    #[must_use]
    pub fn age(&self) -> Duration {
        self.acquired_at.elapsed()
    }

    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.age() >= self.max_pin_duration
    }

    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    pub fn checked_connection(&self) -> Result<&DbConnection> {
        if self.is_expired() {
            self.poisoned.store(true, Ordering::Release);
        }

        if self.is_poisoned() {
            return Err(DbError::MalformedRow {
                operation: DbOperation::Query,
                message: format!(
                    "snapshot pin {} was poisoned or expired by the read-pool lifecycle watchdog; release it and acquire a fresh snapshot",
                    self.pin_id
                        .map(|pin_id| pin_id.to_string())
                        .unwrap_or_else(|| "<unpinned>".to_string())
                ),
            });
        }

        Ok(self.connection().deref())
    }

    pub fn commit(mut self) -> Result<()> {
        if self.snapshot_active {
            self.connection().commit_read_snapshot()?;
            self.snapshot_active = false;
            self.unregister_pin();
        }
        Ok(())
    }

    pub fn rollback(mut self) -> Result<()> {
        if self.snapshot_active {
            self.snapshot_active = false;
            self.unregister_pin();
            if let Some(connection) = self.connection.as_mut() {
                if let Err(error) = connection.rollback_read_snapshot() {
                    connection.abandon();
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn rollback_on_drop(&mut self) {
        if !self.snapshot_active {
            return;
        }

        self.snapshot_active = false;
        self.unregister_pin();
        if let Some(connection) = self.connection.as_mut() {
            if connection.rollback_read_snapshot().is_err() {
                connection.abandon();
            }
        }
    }

    fn unregister_pin(&mut self) {
        if let (Some(pin_id), Some(connection)) = (self.pin_id.take(), self.connection.as_ref())
            && let Some(pool) = connection.pool
        {
            pool.unregister_pin(pin_id);
        }
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
            match (self.pool, self.slot_id) {
                (Some(pool), Some(slot_id)) => pool.release(slot_id, connection),
                _ => {
                    let _ = connection.close();
                }
            }
        }
    }
}

impl Drop for SnapshotPin<'_> {
    fn drop(&mut self) {
        self.rollback_on_drop();
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

fn record_acquire_wait(state: &mut PoolState, duration: Duration) {
    if state.acquire_wait_ns.len() == ACQUIRE_WAIT_SAMPLE_CAP {
        state.acquire_wait_ns.remove(0);
    }
    state.acquire_wait_ns.push(duration.as_nanos());
}

fn acquire_wait_stats(samples: &[u128]) -> AcquireWaitStats {
    if samples.is_empty() {
        return AcquireWaitStats::default();
    }

    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let p50_index = sorted.len() / 2;
    let p99_index = sorted.len().saturating_mul(99).saturating_sub(1) / 100;
    AcquireWaitStats {
        samples: sorted.len(),
        p50_ns: sorted[p50_index],
        p99_ns: sorted[p99_index.min(sorted.len() - 1)],
    }
}

fn expired_pin_count(active_pins: &BTreeMap<u64, ActivePinRecord>) -> usize {
    let now = Instant::now();
    active_pins
        .values()
        .filter(|record| {
            record.poisoned.load(Ordering::Acquire)
                || now
                    .checked_duration_since(record.acquired_at)
                    .unwrap_or(Duration::ZERO)
                    >= record.max_pin_duration
        })
        .count()
}

fn drop_idle_connections(connections: Vec<DbConnection>) {
    for connection in connections {
        let _ = connection.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Barrier};
    use std::thread;

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

    fn join_reader_latency(handle: thread::JoinHandle<u128>) -> u128 {
        match handle.join() {
            Ok(value) => value,
            Err(payload) => {
                if let Some(message) = payload.downcast_ref::<&str>() {
                    panic!("reader thread panicked: {message}");
                }
                if let Some(message) = payload.downcast_ref::<String>() {
                    panic!("reader thread panicked: {message}");
                }
                panic!("reader thread panicked with non-string payload");
            }
        }
    }

    fn p50_latency_ms(values: &[u128]) -> u128 {
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        sorted[sorted.len() / 2]
    }

    fn pool_size_eight_batch_completion_latencies(
        database_path: PathBuf,
        readers: usize,
        per_reader_work: Duration,
    ) -> Vec<u128> {
        let pool = Arc::new(ReadConnectionPool::new(
            DatabaseConfig::file(database_path.clone()),
            PoolConfig::new(8, Duration::from_secs(30)),
        ));
        let readers_ready = Arc::new(Barrier::new(readers + 1));
        let release_readers = Arc::new(Barrier::new(readers + 1));
        let batch_start = Arc::new(Mutex::new(None::<Instant>));

        let handles: Vec<_> = (0..readers)
            .map(|_| {
                let pool = Arc::clone(&pool);
                let readers_ready = Arc::clone(&readers_ready);
                let release_readers = Arc::clone(&release_readers);
                let batch_start = Arc::clone(&batch_start);
                thread::spawn(move || {
                    let pin = must(pool.pin_snapshot(), "fanout reader snapshot opens");
                    assert_eq!(snapshot_item_count(&pin), 1);
                    readers_ready.wait();
                    release_readers.wait();
                    thread::sleep(per_reader_work);
                    assert_eq!(snapshot_item_count(&pin), 1);
                    batch_start
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .expect("batch start set before readers release")
                        .elapsed()
                        .as_millis()
                })
            })
            .collect();

        readers_ready.wait();
        insert_snapshot_item(&database_path, 2, "during_pool_eight_readers");
        {
            let mut start = batch_start
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *start = Some(Instant::now());
        }
        release_readers.wait();

        let latencies: Vec<u128> = handles.into_iter().map(join_reader_latency).collect();
        let fresh = must(pool.acquire(), "fresh reader opens after fanout batch");
        assert_eq!(snapshot_item_count(&fresh), 2);
        latencies
    }

    fn pool_size_one_batch_completion_latencies(
        database_path: PathBuf,
        readers: usize,
        per_reader_work: Duration,
    ) -> Vec<u128> {
        let pool = ReadConnectionPool::new(
            DatabaseConfig::file(database_path.clone()),
            PoolConfig::new(1, Duration::from_secs(30)),
        );
        let batch_start = Instant::now();
        let mut latencies = Vec::with_capacity(readers);

        for index in 0..readers {
            let pin = must(pool.pin_snapshot(), "serial reader snapshot opens");
            assert!(snapshot_item_count(&pin) >= 1);
            thread::sleep(per_reader_work);
            latencies.push(batch_start.elapsed().as_millis());
            drop(pin);
            if index == 0 {
                insert_snapshot_item(&database_path, 2, "during_pool_one_readers");
            }
        }

        latencies
    }

    #[test]
    fn happy_path_pool_acquire_returns_distinct_connections_up_to_cap() {
        let pool = ReadConnectionPool::new(
            DatabaseConfig::memory(),
            PoolConfig::new(2, Duration::from_secs(30)).with_acquire_timeout(Duration::ZERO),
        );

        let first = must(pool.acquire(), "first connection opens");
        let second = must(pool.acquire(), "second connection opens");

        assert_ne!(first.slot_id(), second.slot_id());
        assert_eq!(
            pool.stats(),
            PoolStats {
                active: 2,
                idle: 0,
                active_pins: 0,
                expired_pins: 0,
                max_size: 2,
                max_seen: 2,
                drops: 0,
                ad_hoc_bypass_count: 0,
                acquire_wait: pool.stats().acquire_wait,
                size_was_zero: false,
            }
        );

        let third = must(pool.acquire(), "cap timeout opens ad-hoc connection");
        assert!(third.is_ad_hoc());
        assert_eq!(pool.stats().ad_hoc_bypass_count, 1);
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
        assert_eq!(pool.stats().active_pins, 1);
        must(pin.commit(), "read snapshot commits");

        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.active_pins, 0);
    }

    #[test]
    fn drop_snapshot_pin_abandons_connection_when_rollback_fails() {
        let (_tempdir, _database_path, pool) = file_pool(1);
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");
        must(
            pin.connection().commit_read_snapshot(),
            "test commits behind pin",
        );

        drop(pin);

        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 0);
        assert_eq!(stats.drops, 1);
        let fresh = must(pool.acquire(), "fresh connection opens after abandon");
        assert_ne!(fresh.slot_id(), Some(1));
    }

    #[test]
    fn drop__on_panic_path_releases_pin_idempotently() {
        let (_tempdir, _database_path, pool) = file_pool(1);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _pin = must(pool.pin_snapshot(), "snapshot pin opens before panic");
            assert_eq!(pool.stats().active, 1);
            assert_eq!(pool.stats().active_pins, 1);
            panic!("force unwind across SnapshotPin");
        }));

        assert!(result.is_err());
        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.active_pins, 0);
        assert_eq!(stats.drops, 0);
    }

    #[test]
    fn drop__double_drop_does_not_double_release() {
        let (_tempdir, _database_path, pool) = file_pool(1);
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");
        let slot_id = pin.slot_id();

        must(pin.rollback(), "read snapshot rolls back explicitly");

        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.active_pins, 0);
        assert_eq!(stats.drops, 0);

        let reacquired = must(pool.acquire(), "rolled back pin returns connection once");
        assert_eq!(reacquired.slot_id(), slot_id);
        assert_eq!(pool.stats().active, 1);
        assert_eq!(pool.stats().idle, 0);
    }

    #[test]
    fn drop__field_order_releases_pin_before_returning_connection_under_panic() {
        let (_tempdir, _database_path, pool) = file_pool(1);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _pin = must(pool.pin_snapshot(), "snapshot pin opens before panic");
            panic!("force unwind across SnapshotPin");
        }));

        assert!(result.is_err());

        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.active_pins, 0);

        let _reacquired = must(
            pool.acquire(),
            "connection is reusable only after pin metadata is cleared",
        );
        assert_eq!(pool.stats().active_pins, 0);
    }

    #[test]
    fn rollback_error_abandons_connection_and_prevents_lifo_reuse() {
        let (_tempdir, _database_path, pool) = file_pool(1);
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");
        let slot_id = pin.slot_id();
        must(
            pin.connection().commit_read_snapshot(),
            "test commits behind pin before explicit rollback",
        );

        let error = match pin.rollback() {
            Ok(()) => panic!("rollback after manual commit should fail"),
            Err(error) => error,
        };
        assert!(error.operation().is_some());

        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 0);
        assert_eq!(stats.active_pins, 0);
        assert_eq!(stats.drops, 1);

        let fresh = must(
            pool.acquire(),
            "fresh connection opens after rollback abandon",
        );
        assert_ne!(fresh.slot_id(), slot_id);
    }

    #[test]
    fn cancel_during_wait__no_connection_acquired_no_leak() {
        let pool = ReadConnectionPool::new(
            DatabaseConfig::memory(),
            PoolConfig::new(1, Duration::from_secs(30)).with_acquire_timeout(Duration::ZERO),
        );
        let first = must(pool.acquire(), "first pooled connection opens");

        let timed_out = must(pool.acquire(), "timed-out acquire opens ad-hoc connection");
        assert!(timed_out.is_ad_hoc());
        let stats = pool.stats();
        assert_eq!(stats.active, 1);
        assert_eq!(stats.idle, 0);
        assert_eq!(stats.active_pins, 0);
        assert_eq!(stats.ad_hoc_bypass_count, 1);

        drop(timed_out);
        assert_eq!(pool.stats().active, 1);

        drop(first);
        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.active_pins, 0);
    }

    #[test]
    fn cancel_just_after_acquire__connection_returns_to_lifo_no_pin_state() {
        let pool = memory_pool(1, Duration::from_secs(30));
        let acquired = must(pool.acquire(), "pooled connection opens");
        let slot_id = acquired.slot_id();

        drop(acquired);

        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.active_pins, 0);

        let reacquired = must(pool.acquire(), "dropped connection returns to LIFO");
        assert_eq!(reacquired.slot_id(), slot_id);
    }

    #[test]
    fn cancel_during_pin__rollback_runs_connection_returns_to_lifo() {
        let (_tempdir, database_path, pool) = file_pool(1);
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");
        let slot_id = pin.slot_id();
        assert_eq!(snapshot_item_count(&pin), 1);

        insert_snapshot_item(&database_path, 2, "during_pin");
        drop(pin);

        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.active_pins, 0);

        let reacquired = must(pool.acquire(), "rolled-back pin returns connection");
        assert_eq!(reacquired.slot_id(), slot_id);
        assert_eq!(snapshot_item_count(&reacquired), 2);
    }

    #[test]
    fn watchdog_pin_within_max_duration_is_not_disturbed() {
        let (_tempdir, _database_path, pool) = file_pool(1);
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");

        assert!(pool.expire_stale_pins().is_empty());
        assert!(!pin.is_poisoned());
        assert_eq!(pool.stats().active_pins, 1);
        assert_eq!(pool.stats().expired_pins, 0);
    }

    #[test]
    fn watchdog_pin_held_beyond_max_duration_is_marked_poisoned() {
        let tempdir = must(tempfile::tempdir(), "tempdir creates");
        let database_path = tempdir.path().join("read-pool-expired-pin.db");
        seed_snapshot_database(&database_path);
        let pool = ReadConnectionPool::new(
            DatabaseConfig::file(database_path),
            PoolConfig::new(1, Duration::from_secs(30)).with_max_pin_duration(Duration::ZERO),
        );
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");
        let slot_id = pin.slot_id();

        let expired = pool.expire_stale_pins();

        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].slot_id, slot_id);
        assert!(pin.is_poisoned());
        assert_eq!(pool.stats().active_pins, 1);
        assert_eq!(pool.stats().expired_pins, 1);

        drop(pin);
        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.active_pins, 0);
    }

    #[test]
    fn watchdog_expired_pin_is_reported_once_until_released() {
        let tempdir = must(tempfile::tempdir(), "tempdir creates");
        let database_path = tempdir.path().join("read-pool-expired-pin-once.db");
        seed_snapshot_database(&database_path);
        let pool = ReadConnectionPool::new(
            DatabaseConfig::file(database_path),
            PoolConfig::new(1, Duration::from_secs(30)).with_max_pin_duration(Duration::ZERO),
        );
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");

        let first_scan = pool.expire_stale_pins();
        let second_scan = pool.expire_stale_pins();

        assert_eq!(first_scan.len(), 1);
        assert!(second_scan.is_empty());
        assert!(pin.is_poisoned());
        assert_eq!(pool.stats().active_pins, 1);
        assert_eq!(pool.stats().expired_pins, 1);

        drop(pin);
        assert_eq!(pool.stats().active_pins, 0);
        assert_eq!(pool.stats().expired_pins, 0);
    }

    #[test]
    fn workspace_close_force_poisons_pins_after_drain_timeout() {
        let (_tempdir, _database_path, pool) = file_pool(2);
        let first = must(pool.pin_snapshot(), "first snapshot pin opens");
        let second = must(pool.pin_snapshot(), "second snapshot pin opens");
        let first_slot = first.slot_id();
        let second_slot = second.slot_id();

        let poisoned = pool.force_poison_active_pins();

        assert_eq!(poisoned.len(), 2);
        assert_eq!(poisoned[0].slot_id, first_slot);
        assert_eq!(poisoned[1].slot_id, second_slot);
        assert!(poisoned[0].pin_id < poisoned[1].pin_id);
        assert!(first.is_poisoned());
        assert!(second.is_poisoned());
        assert_eq!(pool.stats().active_pins, 2);
        assert_eq!(pool.stats().expired_pins, 2);

        drop(first);
        drop(second);
        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 2);
        assert_eq!(stats.active_pins, 0);
        assert_eq!(stats.expired_pins, 0);
    }

    #[test]
    fn workspace_close_drains_readers_before_writer_shutdown() {
        let tempdir = must(tempfile::tempdir(), "tempdir creates");
        let database_path = tempdir.path().join("read-pool-drain.db");
        seed_snapshot_database(&database_path);
        let pool = Arc::new(ReadConnectionPool::new(
            DatabaseConfig::file(database_path),
            PoolConfig::new(1, Duration::from_secs(30)),
        ));
        let pin_acquired = Arc::new(Barrier::new(2));

        let reader = {
            let pool = Arc::clone(&pool);
            let pin_acquired = Arc::clone(&pin_acquired);
            thread::spawn(move || {
                let pin = must(pool.pin_snapshot(), "reader snapshot pin opens");
                assert_eq!(snapshot_item_count(&pin), 1);
                pin_acquired.wait();
                thread::sleep(Duration::from_millis(10));
                drop(pin);
            })
        };

        pin_acquired.wait();
        let report = pool.drain_snapshot_pins(Duration::from_secs(1));
        must(reader.join().map_err(|_| "reader panicked"), "reader joins");

        assert!(report.drained);
        assert_eq!(report.active_pins_remaining, 0);
        assert!(report.force_poisoned.is_empty());
        assert_eq!(pool.stats().active_pins, 0);
        assert_eq!(pool.stats().active, 0);
        assert_eq!(pool.stats().idle, 1);
    }

    #[test]
    fn workspace_close_force_poisons_pins_when_drain_timeout_elapses() {
        let (_tempdir, _database_path, pool) = file_pool(1);
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");
        let slot_id = pin.slot_id();

        let report = pool.drain_snapshot_pins(Duration::ZERO);

        assert!(!report.drained);
        assert_eq!(report.active_pins_remaining, 1);
        assert_eq!(report.force_poisoned.len(), 1);
        assert_eq!(report.force_poisoned[0].slot_id, slot_id);
        assert!(pin.is_poisoned());
        assert_eq!(pool.stats().expired_pins, 1);

        drop(pin);
        assert_eq!(pool.stats().active_pins, 0);
    }

    #[test]
    fn poisoned_snapshot_pin_checked_connection_returns_clean_error() {
        let (_tempdir, _database_path, pool) = file_pool(1);
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");

        assert_eq!(
            snapshot_item_count(must(pin.checked_connection(), "pin is usable")),
            1
        );
        let poisoned = pool.force_poison_active_pins();
        assert_eq!(poisoned.len(), 1);

        let error = match pin.checked_connection() {
            Ok(_) => panic!("poisoned pin should not return checked connection"),
            Err(error) => error,
        };
        assert_eq!(error.operation(), Some(DbOperation::Query));
        assert!(error.to_string().contains("snapshot pin"));
        assert!(error.to_string().contains("poisoned"));
        assert!(pin.is_poisoned());

        drop(pin);
        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.active_pins, 0);
    }

    #[test]
    fn checked_connection_expires_over_age_pin_without_pool_scan() {
        let tempdir = must(tempfile::tempdir(), "tempdir creates");
        let database_path = tempdir.path().join("read-pool-checked-expiry.db");
        seed_snapshot_database(&database_path);
        let pool = ReadConnectionPool::new(
            DatabaseConfig::file(database_path),
            PoolConfig::new(1, Duration::from_secs(30)).with_max_pin_duration(Duration::ZERO),
        );
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");

        let error = match pin.checked_connection() {
            Ok(_) => panic!("expired pin should not return checked connection"),
            Err(error) => error,
        };
        assert_eq!(error.operation(), Some(DbOperation::Query));
        assert!(error.to_string().contains("expired"));
        assert!(pin.is_poisoned());
        assert_eq!(pool.stats().active_pins, 1);
        assert_eq!(pool.stats().expired_pins, 1);

        drop(pin);
        assert_eq!(pool.stats().active_pins, 0);
    }

    #[test]
    fn snapshot_pin_reports_age_and_expiry_against_configured_limit() {
        let pool = ReadConnectionPool::new(
            DatabaseConfig::memory(),
            PoolConfig::new(1, Duration::from_secs(30))
                .with_max_pin_duration(Duration::from_secs(60)),
        );
        let pin = must(pool.pin_snapshot(), "snapshot pin opens");

        assert!(!pin.is_expired());
        assert!(pin.age() < Duration::from_secs(60));
    }

    #[test]
    fn empty_or_boundary_pool_size_zero_falls_back_to_size_one_with_warning() {
        let config = PoolConfig::new(0, Duration::from_secs(30));
        assert_eq!(config.requested_max_size(), 0);
        assert_eq!(config.max_size(), 1);
        assert!(config.size_was_zero());

        let pool = ReadConnectionPool::new(
            DatabaseConfig::memory(),
            config.with_acquire_timeout(Duration::ZERO),
        );
        let _first = must(pool.acquire(), "normalized first acquire opens");
        let second = must(pool.acquire(), "normalized pool opens ad-hoc on timeout");
        assert!(second.is_ad_hoc());

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

    #[test]
    fn in_process_fanout_pool_size_eight_holds_eight_stable_snapshots_while_writer_commits() {
        let (_tempdir, database_path, pool) = file_pool(8);
        let readers = 8usize;

        let pins: Vec<_> = (0..readers)
            .map(|_| must(pool.pin_snapshot(), "reader snapshot pin opens"))
            .collect();
        let mut records: Vec<(u64, i64)> = pins
            .iter()
            .map(|pin| {
                (
                    pin.slot_id().expect("fanout pins are pooled"),
                    snapshot_item_count(pin),
                )
            })
            .collect();
        records.sort_by_key(|(slot_id, _)| *slot_id);

        assert_eq!(records.len(), readers);
        let unique_slots: BTreeSet<u64> = records.iter().map(|(slot_id, _)| *slot_id).collect();
        assert_eq!(unique_slots.len(), readers);
        for (_slot_id, before) in &records {
            assert_eq!(*before, 1);
        }

        insert_snapshot_item(&database_path, 2, "after");

        for pin in &pins {
            assert_eq!(snapshot_item_count(pin), 1);
        }

        drop(pins);
        let fresh = must(pool.acquire(), "fresh connection opens after fanout");
        assert_eq!(snapshot_item_count(&fresh), 2);
        drop(fresh);

        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.max_seen, readers);
        assert!(stats.idle <= readers);
    }

    #[test]
    fn acquire_timeout_emits_stats_and_serves_via_ad_hoc_connection() {
        let tempdir = must(tempfile::tempdir(), "tempdir creates");
        let database_path = tempdir.path().join("read-pool-ad-hoc.db");
        seed_snapshot_database(&database_path);
        let pool = ReadConnectionPool::new(
            DatabaseConfig::file(database_path),
            PoolConfig::new(1, Duration::from_secs(30)).with_acquire_timeout(Duration::ZERO),
        );
        let first = must(pool.pin_snapshot(), "first snapshot pin opens");

        let second = must(
            pool.pin_snapshot(),
            "pool_size=1 timeout should open ad-hoc snapshot",
        );
        assert_eq!(second.slot_id(), None);
        assert!(second.connection().is_ad_hoc());
        assert_eq!(snapshot_item_count(&second), 1);
        assert_eq!(pool.stats().active, 1);
        assert_eq!(pool.stats().ad_hoc_bypass_count, 1);
        assert_eq!(pool.stats().acquire_wait.samples, 2);

        drop(first);
        drop(second);
        assert_eq!(pool.stats().active, 0);
        assert_eq!(pool.stats().idle, 1);
    }

    #[test]
    fn acquire_timeout_ad_hoc_connection_is_dropped_after_use() {
        let tempdir = must(tempfile::tempdir(), "tempdir creates");
        let database_path = tempdir.path().join("read-pool-ad-hoc-drop.db");
        seed_snapshot_database(&database_path);
        let pool = ReadConnectionPool::new(
            DatabaseConfig::file(database_path),
            PoolConfig::new(1, Duration::from_secs(30)).with_acquire_timeout(Duration::ZERO),
        );
        let first = must(pool.acquire(), "first pooled connection opens");
        let ad_hoc = must(pool.acquire(), "ad-hoc connection opens");
        assert!(ad_hoc.is_ad_hoc());
        drop(ad_hoc);

        let stats = pool.stats();
        assert_eq!(stats.active, 1);
        assert_eq!(stats.idle, 0);
        assert_eq!(stats.drops, 0);
        assert_eq!(stats.ad_hoc_bypass_count, 1);

        drop(first);
        assert_eq!(pool.stats().idle, 1);
    }

    #[test]
    fn metrics_acquire_wait_histogram_records_p50_and_p99() {
        let samples = acquire_wait_stats(&[10, 30, 20, 50, 40]);

        assert_eq!(samples.samples, 5);
        assert_eq!(samples.p50_ns, 30);
        assert_eq!(samples.p99_ns, 50);
    }

    #[test]
    fn in_process_fanout_latency_pool_eight_meets_speedup_budget_under_writer_load() {
        let readers = 8;
        let per_reader_work = Duration::from_millis(15);
        let (_single_tempdir, single_database_path, _single_pool) = file_pool(1);
        let (_fanout_tempdir, fanout_database_path, _fanout_pool) = file_pool(8);

        let single_latencies = pool_size_one_batch_completion_latencies(
            single_database_path,
            readers,
            per_reader_work,
        );
        let fanout_latencies = pool_size_eight_batch_completion_latencies(
            fanout_database_path,
            readers,
            per_reader_work,
        );
        let single_p50 = p50_latency_ms(&single_latencies);
        let fanout_p50 = p50_latency_ms(&fanout_latencies);

        assert!(
            fanout_p50.saturating_mul(100) <= single_p50.saturating_mul(60),
            "pool_size=8 p50 {fanout_p50}ms should be <= 60% of pool_size=1 p50 {single_p50}ms; single={single_latencies:?} fanout={fanout_latencies:?}",
        );
    }
}
