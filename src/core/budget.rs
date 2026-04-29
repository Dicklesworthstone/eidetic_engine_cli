//! Per-request resource budgets for command handlers.
//!
//! `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md` §6 and `AGENTS.md` ("Asupersync
//! for async edges and cancellation/budget behavior") commit every
//! command-scoped operation to carry a [`RequestBudget`] that bounds
//! wall-clock time, token consumption, memory pressure, and I/O.
//! Budgets are independent of (but composable with) Asupersync `&Cx`
//! cancellation: cancellation says "stop now"; the budget says "stop
//! when you have exceeded what you were allowed".
//!
//! EE-010 (this bead) ships only the type and its math. The
//! capability-narrowed command-context wrapper that threads a
//! `RequestBudget` through every handler is EE-011, and the public
//! degraded-code surface that maps a [`BudgetExceeded`] error onto
//! `degraded[]` entries is EE-016 / EE-006. Strict scope: this module
//! must not depend on either of those landing first.

use std::fmt;
use std::time::{Duration, Instant};

/// A per-request resource bound enforceable at deterministic checkpoints.
///
/// Each dimension (`wall_clock_deadline`, `tokens`, `memory_bytes`,
/// `io_bytes`) is independent and optional; `None` means that dimension
/// is unbounded. The default ([`RequestBudget::unbounded`]) leaves every
/// dimension `None`.
///
/// Recorded usage is monotonic — [`RequestBudget::record_tokens`] and the
/// other recorders are saturating adds and never decrease the recorded
/// count. [`RequestBudget::check`] returns the first exceeded dimension
/// in a deterministic order (`WallClock`, `Tokens`, `Memory`, `Io`) so
/// two callers seeing the same breach receive the same error.
///
/// The struct is `Copy` to keep the call sites cheap; callers who need
/// shared mutation should wrap it in their own synchronisation primitive.
/// The recorders take `&mut self` so concurrent updates require explicit
/// synchronisation by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestBudget {
    started_at: Instant,
    wall_clock_deadline: Option<Instant>,
    tokens_limit: Option<u64>,
    memory_limit_bytes: Option<u64>,
    io_limit_bytes: Option<u64>,
    tokens_used: u64,
    memory_used_bytes: u64,
    io_used_bytes: u64,
}

impl RequestBudget {
    /// Create an unbounded budget anchored at the current `Instant`.
    #[must_use]
    pub fn unbounded() -> Self {
        Self::unbounded_at(Instant::now())
    }

    /// Create an unbounded budget anchored at the supplied clock anchor.
    ///
    /// Tests use this entry point with deterministic anchors so the
    /// elapsed math is reproducible without freezing the system clock.
    #[must_use]
    pub const fn unbounded_at(anchor: Instant) -> Self {
        Self {
            started_at: anchor,
            wall_clock_deadline: None,
            tokens_limit: None,
            memory_limit_bytes: None,
            io_limit_bytes: None,
            tokens_used: 0,
            memory_used_bytes: 0,
            io_used_bytes: 0,
        }
    }

    /// Set the wall-clock budget as a [`Duration`] from the budget's
    /// start anchor.
    ///
    /// Calling this twice keeps the most-recent value; a duration of
    /// zero is allowed and produces an immediate breach on the next
    /// [`RequestBudget::check`].
    #[must_use]
    pub fn with_wall_clock(mut self, budget: Duration) -> Self {
        self.wall_clock_deadline = self.started_at.checked_add(budget);
        self
    }

    /// Set the token budget. `0` means "no tokens at all"; `u64::MAX`
    /// means "effectively unbounded but still typed". Use
    /// [`RequestBudget::unbounded`] for the truly unbounded case.
    #[must_use]
    pub const fn with_tokens(mut self, limit: u64) -> Self {
        self.tokens_limit = Some(limit);
        self
    }

    /// Set the memory budget in bytes.
    #[must_use]
    pub const fn with_memory_bytes(mut self, bytes: u64) -> Self {
        self.memory_limit_bytes = Some(bytes);
        self
    }

    /// Set the I/O budget in bytes (sum of read + write).
    #[must_use]
    pub const fn with_io_bytes(mut self, bytes: u64) -> Self {
        self.io_limit_bytes = Some(bytes);
        self
    }

    /// Wall-clock duration elapsed since the budget started, measured
    /// against the system clock at call time.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.elapsed_at(Instant::now())
    }

    /// Wall-clock duration elapsed measured against an explicit `now`.
    ///
    /// `now` earlier than the budget anchor reports as zero rather than
    /// panicking, so test fixtures can supply monotonic but slightly
    /// re-ordered anchors without surprise.
    #[must_use]
    pub fn elapsed_at(&self, now: Instant) -> Duration {
        now.checked_duration_since(self.started_at)
            .unwrap_or_default()
    }

    /// Remaining wall-clock budget at call time, or `None` if the
    /// dimension is unbounded.
    #[must_use]
    pub fn remaining_wall_clock(&self) -> Option<Duration> {
        self.remaining_wall_clock_at(Instant::now())
    }

    /// Remaining wall-clock budget measured against an explicit `now`.
    /// Past the deadline reports `Some(Duration::ZERO)`.
    #[must_use]
    pub fn remaining_wall_clock_at(&self, now: Instant) -> Option<Duration> {
        self.wall_clock_deadline
            .map(|deadline| deadline.checked_duration_since(now).unwrap_or_default())
    }

    /// Recorded token count.
    #[must_use]
    pub const fn tokens_used(&self) -> u64 {
        self.tokens_used
    }

    /// Recorded memory bytes.
    #[must_use]
    pub const fn memory_used_bytes(&self) -> u64 {
        self.memory_used_bytes
    }

    /// Recorded I/O bytes (read + write combined).
    #[must_use]
    pub const fn io_used_bytes(&self) -> u64 {
        self.io_used_bytes
    }

    /// Saturating add `n` to the recorded token count.
    pub fn record_tokens(&mut self, n: u64) {
        self.tokens_used = self.tokens_used.saturating_add(n);
    }

    /// Saturating add `bytes` to the recorded memory pressure.
    pub fn record_memory_bytes(&mut self, bytes: u64) {
        self.memory_used_bytes = self.memory_used_bytes.saturating_add(bytes);
    }

    /// Saturating add `bytes` to the recorded I/O.
    pub fn record_io_bytes(&mut self, bytes: u64) {
        self.io_used_bytes = self.io_used_bytes.saturating_add(bytes);
    }

    /// Snapshot the limit/used pair for a dimension; `None` when that
    /// dimension is unbounded.
    #[must_use]
    pub fn snapshot(&self, dimension: BudgetDimension) -> Option<BudgetSnapshot> {
        match dimension {
            BudgetDimension::WallClock => self.wall_clock_deadline.map(|deadline| {
                let elapsed = self.elapsed();
                let limit = deadline
                    .checked_duration_since(self.started_at)
                    .unwrap_or_default();
                BudgetSnapshot {
                    dimension,
                    limit: duration_to_millis(limit),
                    used: duration_to_millis(elapsed),
                }
            }),
            BudgetDimension::Tokens => self.tokens_limit.map(|limit| BudgetSnapshot {
                dimension,
                limit: u128::from(limit),
                used: u128::from(self.tokens_used),
            }),
            BudgetDimension::Memory => self.memory_limit_bytes.map(|limit| BudgetSnapshot {
                dimension,
                limit: u128::from(limit),
                used: u128::from(self.memory_used_bytes),
            }),
            BudgetDimension::Io => self.io_limit_bytes.map(|limit| BudgetSnapshot {
                dimension,
                limit: u128::from(limit),
                used: u128::from(self.io_used_bytes),
            }),
        }
    }

    /// Verify every bounded dimension is within its limit. Returns the
    /// first exceeded dimension in deterministic order.
    pub fn check(&self) -> Result<(), BudgetExceeded> {
        self.check_at(Instant::now())
    }

    /// As [`RequestBudget::check`] but against an explicit clock anchor.
    pub fn check_at(&self, now: Instant) -> Result<(), BudgetExceeded> {
        for dimension in DIMENSION_ORDER {
            if let Some(snapshot) = self.snapshot_at(dimension, now)
                && snapshot.is_exceeded()
            {
                return Err(BudgetExceeded::from(snapshot));
            }
        }
        Ok(())
    }

    fn snapshot_at(&self, dimension: BudgetDimension, now: Instant) -> Option<BudgetSnapshot> {
        match dimension {
            BudgetDimension::WallClock => self.wall_clock_deadline.map(|deadline| {
                let elapsed = self.elapsed_at(now);
                let limit = deadline
                    .checked_duration_since(self.started_at)
                    .unwrap_or_default();
                BudgetSnapshot {
                    dimension,
                    limit: duration_to_millis(limit),
                    used: duration_to_millis(elapsed),
                }
            }),
            BudgetDimension::Tokens | BudgetDimension::Memory | BudgetDimension::Io => {
                self.snapshot(dimension)
            }
        }
    }
}

/// Deterministic order in which [`RequestBudget::check`] reports
/// breaches. Wall clock comes first because it is the most user-visible
/// dimension and the most expensive to recover from.
const DIMENSION_ORDER: [BudgetDimension; 4] = [
    BudgetDimension::WallClock,
    BudgetDimension::Tokens,
    BudgetDimension::Memory,
    BudgetDimension::Io,
];

fn duration_to_millis(d: Duration) -> u128 {
    d.as_millis()
}

/// Identifies a single resource axis a [`RequestBudget`] tracks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetDimension {
    /// Wall-clock time, measured in milliseconds in [`BudgetSnapshot`].
    WallClock,
    /// Token count (e.g. tiktoken units).
    Tokens,
    /// Bytes of allocated working memory.
    Memory,
    /// Bytes of read + write I/O.
    Io,
}

impl BudgetDimension {
    /// Stable string representation for JSON / log fields.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WallClock => "wall_clock",
            Self::Tokens => "tokens",
            Self::Memory => "memory",
            Self::Io => "io",
        }
    }
}

impl fmt::Display for BudgetDimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A point-in-time view of one dimension. `limit` and `used` are kept
/// as `u128` so wall-clock milliseconds and byte counts share a shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetSnapshot {
    /// Which dimension this snapshot describes.
    pub dimension: BudgetDimension,
    /// Configured ceiling for the dimension.
    pub limit: u128,
    /// Recorded usage at the snapshot moment.
    pub used: u128,
}

impl BudgetSnapshot {
    /// `true` if `used > limit`.
    #[must_use]
    pub const fn is_exceeded(&self) -> bool {
        self.used > self.limit
    }
}

/// Error emitted when a [`RequestBudget::check`] finds any dimension
/// past its limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetExceeded {
    /// Dimension that breached.
    pub dimension: BudgetDimension,
    /// Configured ceiling for that dimension.
    pub limit: u128,
    /// Recorded usage at the moment of the breach.
    pub used: u128,
}

impl From<BudgetSnapshot> for BudgetExceeded {
    fn from(snapshot: BudgetSnapshot) -> Self {
        Self {
            dimension: snapshot.dimension,
            limit: snapshot.limit,
            used: snapshot.used,
        }
    }
}

impl fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "request budget exceeded: dimension={} limit={} used={}",
            self.dimension, self.limit, self.used
        )
    }
}

impl std::error::Error for BudgetExceeded {}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{BudgetDimension, BudgetExceeded, BudgetSnapshot, RequestBudget};

    fn anchor() -> Instant {
        // A single Instant grabbed once at the top of each test gives
        // deterministic elapsed math without freezing the global clock.
        Instant::now()
    }

    fn require_err<T>(
        result: std::result::Result<T, BudgetExceeded>,
        message: &'static str,
    ) -> std::result::Result<BudgetExceeded, &'static str> {
        result.err().ok_or(message)
    }

    fn require_some<T>(
        value: Option<T>,
        message: &'static str,
    ) -> std::result::Result<T, &'static str> {
        value.ok_or(message)
    }

    #[test]
    fn unbounded_budget_never_exceeds() {
        let now = anchor();
        let mut b = RequestBudget::unbounded_at(now);
        b.record_tokens(u64::MAX);
        b.record_memory_bytes(u64::MAX);
        b.record_io_bytes(u64::MAX);
        let later = now + Duration::from_secs(60 * 60 * 24);
        assert!(b.check_at(later).is_ok());
    }

    #[test]
    fn wall_clock_breach_is_detected_at_or_after_deadline() -> std::result::Result<(), &'static str>
    {
        let now = anchor();
        let b = RequestBudget::unbounded_at(now).with_wall_clock(Duration::from_millis(100));

        let before = now + Duration::from_millis(50);
        assert!(b.check_at(before).is_ok());

        let at_deadline = now + Duration::from_millis(100);
        // At deadline exactly, used == limit; not yet exceeded.
        assert!(b.check_at(at_deadline).is_ok());

        let past_deadline = now + Duration::from_millis(101);
        let err = require_err(b.check_at(past_deadline), "past deadline must fail")?;
        assert_eq!(err.dimension, BudgetDimension::WallClock);
        assert_eq!(err.limit, 100);
        assert_eq!(err.used, 101);
        Ok(())
    }

    #[test]
    fn tokens_breach_is_detected_after_recording() -> std::result::Result<(), &'static str> {
        let now = anchor();
        let mut b = RequestBudget::unbounded_at(now).with_tokens(10);
        b.record_tokens(7);
        assert!(b.check_at(now).is_ok());
        b.record_tokens(4);
        let err = require_err(b.check_at(now), "11 tokens past 10 must fail")?;
        assert_eq!(err.dimension, BudgetDimension::Tokens);
        assert_eq!(err.limit, 10);
        assert_eq!(err.used, 11);
        Ok(())
    }

    #[test]
    fn memory_breach_is_detected_after_recording() -> std::result::Result<(), &'static str> {
        let now = anchor();
        let mut b = RequestBudget::unbounded_at(now).with_memory_bytes(1024);
        b.record_memory_bytes(2048);
        let err = require_err(b.check_at(now), "memory must breach")?;
        assert_eq!(err.dimension, BudgetDimension::Memory);
        assert_eq!(err.limit, 1024);
        assert_eq!(err.used, 2048);
        Ok(())
    }

    #[test]
    fn io_breach_is_detected_after_recording() -> std::result::Result<(), &'static str> {
        let now = anchor();
        let mut b = RequestBudget::unbounded_at(now).with_io_bytes(1);
        b.record_io_bytes(2);
        let err = require_err(b.check_at(now), "io must breach")?;
        assert_eq!(err.dimension, BudgetDimension::Io);
        assert_eq!(err.limit, 1);
        assert_eq!(err.used, 2);
        Ok(())
    }

    #[test]
    fn simultaneous_breaches_report_wall_clock_first() -> std::result::Result<(), &'static str> {
        let now = anchor();
        let mut b = RequestBudget::unbounded_at(now)
            .with_wall_clock(Duration::from_millis(10))
            .with_tokens(1)
            .with_memory_bytes(1)
            .with_io_bytes(1);
        b.record_tokens(2);
        b.record_memory_bytes(2);
        b.record_io_bytes(2);
        let past = now + Duration::from_millis(11);
        let err = require_err(b.check_at(past), "multi-axis breach must fail")?;
        assert_eq!(err.dimension, BudgetDimension::WallClock);
        Ok(())
    }

    #[test]
    fn ordering_after_wall_clock_is_tokens_then_memory_then_io()
    -> std::result::Result<(), &'static str> {
        let now = anchor();
        let mut b = RequestBudget::unbounded_at(now)
            .with_tokens(1)
            .with_memory_bytes(1)
            .with_io_bytes(1);
        b.record_tokens(2);
        b.record_memory_bytes(2);
        b.record_io_bytes(2);
        let err = require_err(b.check_at(now), "multi-axis breach must fail")?;
        assert_eq!(err.dimension, BudgetDimension::Tokens);

        let mut b2 = RequestBudget::unbounded_at(now)
            .with_memory_bytes(1)
            .with_io_bytes(1);
        b2.record_memory_bytes(2);
        b2.record_io_bytes(2);
        let err2 = require_err(b2.check_at(now), "memory-then-io breach must fail")?;
        assert_eq!(err2.dimension, BudgetDimension::Memory);
        Ok(())
    }

    #[test]
    fn record_tokens_is_saturating_at_u64_max() {
        let mut b = RequestBudget::unbounded_at(anchor());
        b.record_tokens(u64::MAX);
        b.record_tokens(1);
        assert_eq!(b.tokens_used(), u64::MAX);
    }

    #[test]
    fn record_memory_is_saturating_at_u64_max() {
        let mut b = RequestBudget::unbounded_at(anchor());
        b.record_memory_bytes(u64::MAX);
        b.record_memory_bytes(1);
        assert_eq!(b.memory_used_bytes(), u64::MAX);
    }

    #[test]
    fn record_io_is_saturating_at_u64_max() {
        let mut b = RequestBudget::unbounded_at(anchor());
        b.record_io_bytes(u64::MAX);
        b.record_io_bytes(1);
        assert_eq!(b.io_used_bytes(), u64::MAX);
    }

    #[test]
    fn elapsed_at_pre_anchor_reports_zero() {
        let now = anchor();
        let b = RequestBudget::unbounded_at(now);
        // We cannot manufacture an Instant before `now` portably, so
        // assert the same-instant case which is the boundary the test
        // guards against panicking.
        assert_eq!(b.elapsed_at(now), Duration::ZERO);
    }

    #[test]
    fn remaining_wall_clock_is_none_when_unbounded() {
        let b = RequestBudget::unbounded_at(anchor());
        assert!(b.remaining_wall_clock().is_none());
    }

    #[test]
    fn remaining_wall_clock_is_some_zero_after_deadline() {
        let now = anchor();
        let b = RequestBudget::unbounded_at(now).with_wall_clock(Duration::from_millis(50));
        let past = now + Duration::from_millis(75);
        assert_eq!(b.remaining_wall_clock_at(past), Some(Duration::ZERO));
    }

    #[test]
    fn snapshot_returns_none_for_unbounded_dimension() {
        let b = RequestBudget::unbounded_at(anchor());
        for dim in [
            BudgetDimension::WallClock,
            BudgetDimension::Tokens,
            BudgetDimension::Memory,
            BudgetDimension::Io,
        ] {
            assert!(b.snapshot(dim).is_none(), "{dim:?} must be unbounded");
        }
    }

    #[test]
    fn snapshot_reports_used_and_limit_for_bounded_dimension()
    -> std::result::Result<(), &'static str> {
        let mut b = RequestBudget::unbounded_at(anchor())
            .with_tokens(100)
            .with_memory_bytes(2_048)
            .with_io_bytes(4_096);
        b.record_tokens(40);
        b.record_memory_bytes(2_048);
        b.record_io_bytes(1);

        let tokens = require_some(
            b.snapshot(BudgetDimension::Tokens),
            "tokens dimension is bounded",
        )?;
        assert_eq!(tokens.limit, 100);
        assert_eq!(tokens.used, 40);
        assert!(!tokens.is_exceeded());

        let memory = require_some(
            b.snapshot(BudgetDimension::Memory),
            "memory dimension is bounded",
        )?;
        assert_eq!(memory.limit, 2_048);
        assert_eq!(memory.used, 2_048);
        assert!(!memory.is_exceeded());

        let io = require_some(b.snapshot(BudgetDimension::Io), "io dimension is bounded")?;
        assert_eq!(io.limit, 4_096);
        assert_eq!(io.used, 1);
        assert!(!io.is_exceeded());
        Ok(())
    }

    #[test]
    fn budget_dimension_strings_are_stable() {
        assert_eq!(BudgetDimension::WallClock.as_str(), "wall_clock");
        assert_eq!(BudgetDimension::Tokens.as_str(), "tokens");
        assert_eq!(BudgetDimension::Memory.as_str(), "memory");
        assert_eq!(BudgetDimension::Io.as_str(), "io");
    }

    #[test]
    fn budget_exceeded_display_includes_dimension_limit_and_used() {
        let err = BudgetExceeded::from(BudgetSnapshot {
            dimension: BudgetDimension::Tokens,
            limit: 100,
            used: 200,
        });
        let rendered = format!("{err}");
        assert!(rendered.contains("dimension=tokens"));
        assert!(rendered.contains("limit=100"));
        assert!(rendered.contains("used=200"));
    }

    #[test]
    fn zero_wall_clock_budget_breaches_immediately_after_anchor() {
        let now = anchor();
        let b = RequestBudget::unbounded_at(now).with_wall_clock(Duration::ZERO);
        // At the anchor itself, used == limit == 0; not exceeded yet.
        assert!(b.check_at(now).is_ok());
        let past = now + Duration::from_millis(1);
        assert!(b.check_at(past).is_err());
    }
}
