//! Deterministic runtime capability token for code paths that must not
//! consume ambient randomness or wall-clock state.
//!
//! The token is intentionally move-only:
//!
//! ```compile_fail
//! use ee::runtime::determinism::Deterministic;
//!
//! let token = Deterministic::from_seed(42);
//! let _clone = token.clone();
//! ```
//!
//! It is also intentionally not `Sync`; deterministic scopes are consumed
//! through mutable access so concurrent paths must split explicit child scopes:
//!
//! ```compile_fail
//! use ee::runtime::determinism::Deterministic;
//!
//! fn assert_sync<T: Sync>() {}
//! assert_sync::<Deterministic>();
//! ```
//!
//! Basic usage:
//!
//! ```
//! use ee::runtime::determinism::Deterministic;
//!
//! let mut token = Deterministic::from_seed(7);
//! let mut retrieval = token.child("retrieval");
//! let first = retrieval.clock().next_uuid_v7();
//! let second = retrieval.clock().next_uuid_v7();
//!
//! assert!(first < second);
//! ```

use std::cell::Cell;
use std::env;
use std::fmt;
use std::marker::PhantomData;

use chrono::{DateTime, Utc};
use uuid::{Timestamp, Uuid};

/// N4.1 inventory hash that drove the first deterministic-token design.
pub const RANDOMNESS_INVENTORY_ROWS_CONTENT_HASH: &str =
    "blake3-ish:51a8854727a5768008ba8269596e8666cc9ffdd88e8ac3f13101ad36434a3bfc";

const ROOT_SCOPE: &str = "root";
const UUID_COUNTER_BITS: u8 = 74;

/// Stable 64-bit seed used by deterministic scopes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Seed(u64);

impl Seed {
    /// Construct a seed from an explicit numeric value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the numeric seed.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Derive a seed from stable bytes and a domain label.
    #[must_use]
    pub fn from_bytes(domain: &str, bytes: impl AsRef<[u8]>) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ee.determinism.seed.v1");
        hasher.update(domain.as_bytes());
        hasher.update(&[0]);
        hasher.update(bytes.as_ref());
        let digest = hasher.finalize();
        let mut seed_bytes = [0_u8; 8];
        seed_bytes.copy_from_slice(&digest.as_bytes()[..8]);
        Self(u64::from_be_bytes(seed_bytes))
    }
}

impl From<u64> for Seed {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

/// Source used to construct a deterministic token.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SeedSource {
    /// User or caller supplied a numeric seed directly.
    Explicit,
    /// Seed was derived from stable workspace state.
    PersistentWorkspace,
    /// Seed was derived from an RFC 3339 timestamp truncated to seconds.
    TimestampSecond,
    /// Seed was read from an environment variable.
    Env,
    /// Seed was derived from a parent token and child label.
    Child,
}

impl SeedSource {
    /// Stable snake-case source name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::PersistentWorkspace => "persistent_workspace",
            Self::TimestampSecond => "timestamp_second",
            Self::Env => "env",
            Self::Child => "child",
        }
    }
}

/// Error returned when deterministic token construction fails.
#[derive(Debug, Eq, PartialEq)]
pub enum DeterminismError {
    /// The requested environment variable is not present.
    MissingEnv { name: String },
    /// A seed value could not be parsed as an unsigned integer.
    InvalidSeed { value: String },
    /// An RFC 3339 timestamp could not be parsed.
    InvalidTimestamp { value: String, message: String },
}

impl fmt::Display for DeterminismError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnv { name } => {
                write!(
                    formatter,
                    "determinism seed environment variable `{name}` is missing"
                )
            }
            Self::InvalidSeed { value } => {
                write!(formatter, "determinism seed `{value}` is not a u64")
            }
            Self::InvalidTimestamp { value, message } => write!(
                formatter,
                "determinism timestamp `{value}` is not valid RFC 3339: {message}"
            ),
        }
    }
}

impl std::error::Error for DeterminismError {}

/// Move-only capability token for deterministic consumers.
///
/// The generic parameter marks the call-site origin at the type level. N4.3
/// threads this token through retrieval, scoring, MMR, pack assembly, and ID
/// construction; N4.2 only introduces the substrate.
#[derive(Debug)]
pub struct Deterministic<S = Seed> {
    seed: Seed,
    source: SeedSource,
    scope: String,
    counter: u64,
    _scope: PhantomData<fn() -> S>,
    _not_sync: PhantomData<Cell<()>>,
}

impl Deterministic<Seed> {
    /// Construct a root token from an explicit numeric seed.
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        Self::from_parts(Seed::new(seed), SeedSource::Explicit, ROOT_SCOPE.to_owned())
    }

    /// Construct a root token from a persistent workspace seed material.
    #[must_use]
    pub fn from_persistent_seed(bytes: impl AsRef<[u8]>) -> Self {
        Self::from_parts(
            Seed::from_bytes("persistent_workspace", bytes),
            SeedSource::PersistentWorkspace,
            ROOT_SCOPE.to_owned(),
        )
    }

    /// Construct a root token from an RFC 3339 timestamp truncated to seconds.
    pub fn from_timestamp_second(value: &str) -> Result<Self, DeterminismError> {
        let parsed = DateTime::parse_from_rfc3339(value).map_err(|error| {
            DeterminismError::InvalidTimestamp {
                value: value.to_owned(),
                message: error.to_string(),
            }
        })?;
        let seconds = parsed.with_timezone(&Utc).timestamp();
        Ok(Self::from_parts(
            Seed::from_bytes("timestamp_second", seconds.to_be_bytes()),
            SeedSource::TimestampSecond,
            ROOT_SCOPE.to_owned(),
        ))
    }

    /// Construct a root token from an environment variable containing a u64.
    pub fn from_env(name: &str) -> Result<Self, DeterminismError> {
        let value = env::var(name).map_err(|_| DeterminismError::MissingEnv {
            name: name.to_owned(),
        })?;
        Self::from_env_value(&value)
    }

    /// Construct a root token from an already-read environment value.
    ///
    /// This is the test-friendly form: tests do not need to mutate process
    /// environment to prove the parser contract.
    pub fn from_env_value(value: &str) -> Result<Self, DeterminismError> {
        let seed = value
            .parse::<u64>()
            .map_err(|_| DeterminismError::InvalidSeed {
                value: value.to_owned(),
            })?;
        Ok(Self::from_parts(
            Seed::new(seed),
            SeedSource::Env,
            ROOT_SCOPE.to_owned(),
        ))
    }
}

impl<S> Deterministic<S> {
    fn from_parts(seed: Seed, source: SeedSource, scope: String) -> Self {
        Self {
            seed,
            source,
            scope,
            counter: 0,
            _scope: PhantomData,
            _not_sync: PhantomData,
        }
    }

    /// Return this token's stable seed.
    #[must_use]
    pub const fn seed(&self) -> Seed {
        self.seed
    }

    /// Return how this token was constructed.
    #[must_use]
    pub const fn source(&self) -> SeedSource {
        self.source
    }

    /// Return the deterministic scope path.
    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Return a short non-secret hash prefix for logs.
    #[must_use]
    pub fn seed_hash_prefix(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ee.determinism.seed_hash_prefix.v1");
        hasher.update(&self.seed.as_u64().to_be_bytes());
        hasher.update(self.scope.as_bytes());
        let digest = hasher.finalize();
        hex_prefix(digest.as_bytes(), 12)
    }

    /// Split this token into a deterministic child scope.
    ///
    /// The same parent seed, parent scope, first-use ordinal, and label produce
    /// the same child seed across runs. Repeated child calls on the same token
    /// remain distinct because the parent ordinal advances.
    #[must_use]
    pub fn child(&mut self, label: &str) -> Deterministic<Seed> {
        let ordinal = self.next_counter();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ee.determinism.child.v1");
        hasher.update(&self.seed.as_u64().to_be_bytes());
        hasher.update(self.scope.as_bytes());
        hasher.update(&[0]);
        hasher.update(label.as_bytes());
        hasher.update(&ordinal.to_be_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&digest.as_bytes()[..8]);
        let child_seed = Seed::new(u64::from_be_bytes(bytes));
        Deterministic::from_parts(
            child_seed,
            SeedSource::Child,
            format!("{}::{label}#{ordinal}", self.scope),
        )
    }

    /// Create a deterministic clock consumer tied to this token.
    pub fn clock(&mut self) -> DeterministicClock<'_, S> {
        DeterministicClock { token: self }
    }

    /// Create a deterministic byte generator tied to this token.
    pub fn rng(&mut self) -> DeterministicRng<'_, S> {
        DeterministicRng { token: self }
    }

    /// Create a deterministic ordering helper tied to this token.
    pub fn order(&mut self) -> DeterministicOrder<'_, S> {
        DeterministicOrder { _token: self }
    }

    fn next_counter(&mut self) -> u64 {
        let current = self.counter;
        self.counter = self.counter.saturating_add(1);
        current
    }

    fn next_word(&mut self, domain: &[u8]) -> u64 {
        let ordinal = self.next_counter();
        splitmix64(self.seed.as_u64() ^ ordinal ^ stable_domain_word(domain))
    }
}

/// Marker trait for deterministic consumers that can only be built from a
/// [`Deterministic`] token.
pub trait RandomnessConsumer {
    /// Stable consumer kind for logs and tests.
    fn consumer_kind(&self) -> &'static str;
}

/// Deterministic clock that produces UUIDv7-compatible timestamps.
pub struct DeterministicClock<'a, S = Seed> {
    token: &'a mut Deterministic<S>,
}

impl<S> DeterministicClock<'_, S> {
    /// Advance the deterministic clock and return a UUID timestamp.
    #[must_use]
    pub fn advance(&mut self) -> Timestamp {
        let ordinal = self.token.next_counter();
        let millis = self.token.seed.as_u64().saturating_add(ordinal);
        let seconds = millis / 1_000;
        let subsec_nanos = ((millis % 1_000) as u32).saturating_mul(1_000_000);
        Timestamp::from_unix_time(seconds, subsec_nanos, ordinal as u128, UUID_COUNTER_BITS)
    }

    /// Advance the deterministic clock and return a UUIDv7 value.
    #[must_use]
    pub fn next_uuid_v7(&mut self) -> Uuid {
        Uuid::new_v7(self.advance())
    }
}

impl<S> RandomnessConsumer for DeterministicClock<'_, S> {
    fn consumer_kind(&self) -> &'static str {
        "deterministic_clock"
    }
}

/// Deterministic byte generator for bounded local consumers.
pub struct DeterministicRng<'a, S = Seed> {
    token: &'a mut Deterministic<S>,
}

impl<S> DeterministicRng<'_, S> {
    /// Return the next deterministic `u64`.
    #[must_use]
    pub fn next_u64(&mut self) -> u64 {
        self.token.next_word(b"rng_u64")
    }

    /// Fill bytes deterministically from this token.
    pub fn fill_bytes(&mut self, output: &mut [u8]) {
        for chunk in output.chunks_mut(8) {
            let word = self.next_u64().to_be_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }
}

impl<S> RandomnessConsumer for DeterministicRng<'_, S> {
    fn consumer_kind(&self) -> &'static str {
        "deterministic_rng"
    }
}

/// Deterministic ordering helper for collections whose native iteration order
/// is not stable enough for machine-facing output.
pub struct DeterministicOrder<'a, S = Seed> {
    _token: &'a mut Deterministic<S>,
}

impl<S> DeterministicOrder<'_, S> {
    /// Sort values by a caller-provided stable key.
    pub fn sort_by_key<T, K: Ord>(&mut self, values: &mut [T], mut key: impl FnMut(&T) -> K) {
        values.sort_by_key(|value| key(value));
    }
}

impl<S> RandomnessConsumer for DeterministicOrder<'_, S> {
    fn consumer_kind(&self) -> &'static str {
        "deterministic_order"
    }
}

fn stable_domain_word(domain: &[u8]) -> u64 {
    let digest = blake3::hash(domain);
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_be_bytes(bytes)
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn hex_prefix(bytes: &[u8], chars: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(chars);
    for byte in bytes {
        if output.len() >= chars {
            break;
        }
        output.push(HEX[(byte >> 4) as usize] as char);
        if output.len() >= chars {
            break;
        }
        output.push(HEX[(byte & 0x0F) as usize] as char);
    }
    output
}
