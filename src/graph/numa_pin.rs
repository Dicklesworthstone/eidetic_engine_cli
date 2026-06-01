//! NUMA-aware mmap'd graph snapshot pinning — scaffold (bd-ldstd, sub-bead of
//! bd-1prrl.3 / swarmx.4).
//!
//! On 2-socket Linux hosts with 256GB+ RAM and 64+ cores, the kernel scatters
//! a freshly-loaded graph snapshot blob's pages across both NUMA nodes. A
//! worker thread running on socket 0 that touches a page resident on socket
//! 1's memory controller pays roughly 2× the cross-node latency penalty per
//! random access; over 10⁸ random accesses (typical for PPR / HITS / k-truss
//! hot loops) that's the difference between an 8s and a 16s wall-clock.
//!
//! This module owns the platform-agnostic public surface for that
//! optimization: configuration types, a deterministic mapping/pinning plan, a
//! result envelope that records what actually happened, the degraded-code
//! vocabulary, and the entry points the `refresh_graph_snapshot` /
//! `load_graph_snapshot` consumers will eventually call. The Linux
//! libc::mbind real-syscall path and the wiring into `src/graph/mod.rs`
//! snapshot loaders are deferred to follow-up slices under bd-1prrl.3.
//!
//! The scaffold is honest: on every non-Linux platform `pin_snapshot_blob`
//! returns a fully populated `NumaPinResult` with `supported=false`, a
//! fallback-path indicator, and the `numa_pin_unsupported_platform` degraded
//! code; on Linux it currently returns `succeeded=false` with the
//! `numa_pin_linux_not_implemented` degraded code so callers cannot mistake
//! the scaffold for a working syscall path. When the operator disables the
//! optimization via `NumaPinConfig::disabled()` the result short-circuits
//! with the `numa_pin_disabled` code regardless of platform.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// `degraded[]` code emitted when the host platform does not expose NUMA
/// primitives that the optimization needs (macOS, Windows, any non-Linux
/// Unix). Build-time classification per `docs/degraded_code_taxonomy.md`.
pub const NUMA_PIN_UNSUPPORTED_PLATFORM_CODE: &str = "numa_pin_unsupported_platform";

/// `degraded[]` code emitted when an operator has disabled the optimization
/// through `[graph.numa_pin] enabled = false` (or its env-var equivalent).
/// Response-time classification per `docs/degraded_code_taxonomy.md`.
pub const NUMA_PIN_DISABLED_CODE: &str = "numa_pin_disabled";

/// `degraded[]` code emitted on Linux while the scaffold ships without the
/// real libc::mbind / MAP_POPULATE syscall path. Tracked under follow-up
/// slices of bd-1prrl.3; consumers MUST treat this exactly like the
/// unsupported-platform path (degrade gracefully, never panic, never claim
/// the snapshot was pinned).
pub const NUMA_PIN_LINUX_NOT_IMPLEMENTED_CODE: &str = "numa_pin_linux_not_implemented";

/// Forward-looking schema id for the `ee status --json` numaPin block, kept
/// in sync with `docs/schemas/ee.status.graph.numa_pin.v1.json`. The wiring
/// slice in bd-1prrl.3 surfaces it through `data.graph.numaPin.schema`.
pub const STATUS_GRAPH_NUMA_PIN_SCHEMA_V1: &str = "ee.status.graph.numa_pin.v1";

/// Stable schema id for the per-snapshot side-table row that later database
/// wiring persists as `graph_snapshot_numa_hints`.
pub const GRAPH_SNAPSHOT_NUMA_HINT_SCHEMA_V1: &str = "ee.graph.snapshot_numa_hint.v1";

/// Operator env var that disables graph snapshot NUMA pinning without editing
/// config. The parsed value is inverted into [`NumaPinConfig::enabled`].
pub const NUMA_PIN_DISABLE_ENV: &str = "EE_GRAPH_NUMA_PIN_DISABLE";

/// Operator env var that selects either `auto` or an explicit non-negative
/// NUMA node id.
pub const NUMA_PIN_NODE_ENV: &str = "EE_GRAPH_NUMA_PIN_NODE";

/// Operator env var that controls whether the loader should pre-fault pages
/// with MAP_POPULATE / the platform fallback.
pub const NUMA_PIN_POPULATE_ENV: &str = "EE_GRAPH_NUMA_PIN_POPULATE";

/// Default NUMA node preference key emitted in `preferredNode` JSON when the
/// operator asked for automatic detection.
pub const NUMA_PIN_PREFERRED_NODE_AUTO: &str = "auto";

/// Coarse host classification for the NUMA optimization. Linux is the only
/// platform that exposes the required primitives today; everything else falls
/// through to the safe-by-construction `Vec<u8>` deserialization path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NumaPinPlatform {
    Linux,
    MacosUnsupported,
    WindowsUnsupported,
    OtherUnsupported,
}

impl NumaPinPlatform {
    #[must_use]
    pub fn detect() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacosUnsupported
        } else if cfg!(target_os = "windows") {
            Self::WindowsUnsupported
        } else {
            Self::OtherUnsupported
        }
    }

    #[must_use]
    pub fn is_supported(self) -> bool {
        matches!(self, Self::Linux)
    }
}

/// Operator-facing NUMA node preference. `Auto` defers to the calling CPU's
/// node via `detect_preferred_node`; `Node(i)` pins to a specific node number
/// (validated lazily by the syscall slice). Validation deliberately stays
/// platform-agnostic at the scaffold layer because non-Linux platforms have
/// no node space to validate against.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NumaPinPreference {
    #[default]
    Auto,
    Node(i32),
}

impl NumaPinPreference {
    #[must_use]
    pub fn as_str(self) -> Cow<'static, str> {
        match self {
            Self::Auto => Cow::Borrowed(NUMA_PIN_PREFERRED_NODE_AUTO),
            Self::Node(node) => Cow::Owned(node.to_string()),
        }
    }
}

/// Configuration knobs for snapshot pinning. Defaults are conservative
/// (`enabled=true`, `Auto` node, `populate_on_load=true`) so that on a
/// supported Linux host the optimization fires without further opt-in, while
/// remaining safe on every other platform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NumaPinConfig {
    pub enabled: bool,
    pub preferred_node: NumaPinPreference,
    pub populate_on_load: bool,
}

impl Default for NumaPinConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            preferred_node: NumaPinPreference::Auto,
            populate_on_load: true,
        }
    }
}

impl NumaPinConfig {
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_preferred_node(mut self, preference: NumaPinPreference) -> Self {
        self.preferred_node = preference;
        self
    }

    #[must_use]
    pub fn with_populate_on_load(mut self, populate: bool) -> Self {
        self.populate_on_load = populate;
        self
    }

    /// Build a graph NUMA config from an arbitrary env-var reader.
    ///
    /// This lives in `src/graph` so the loader/status wiring can share one
    /// parser once the global env registry grows `EE_GRAPH_NUMA_PIN_*`
    /// variants. Missing values keep defaults. Invalid values keep defaults
    /// and are reported through `on_unparseable` so callers can attach a
    /// degraded code without making the parser nondeterministic.
    #[must_use]
    pub fn from_environment_with_reader<F, G>(reader: F, mut on_unparseable: G) -> Self
    where
        F: Fn(&'static str) -> Option<String>,
        G: FnMut(&'static str, &str),
    {
        let mut config = Self::default();

        if let Some(raw) = reader(NUMA_PIN_DISABLE_ENV) {
            match parse_env_bool(&raw) {
                Some(disabled) => config.enabled = !disabled,
                None => on_unparseable(NUMA_PIN_DISABLE_ENV, &raw),
            }
        }

        if let Some(raw) = reader(NUMA_PIN_NODE_ENV) {
            match parse_env_preferred_node(&raw) {
                Some(preference) => config.preferred_node = preference,
                None => on_unparseable(NUMA_PIN_NODE_ENV, &raw),
            }
        }

        if let Some(raw) = reader(NUMA_PIN_POPULATE_ENV) {
            match parse_env_bool(&raw) {
                Some(populate) => config.populate_on_load = populate,
                None => on_unparseable(NUMA_PIN_POPULATE_ENV, &raw),
            }
        }

        config
    }
}

fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_env_preferred_node(raw: &str) -> Option<NumaPinPreference> {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case(NUMA_PIN_PREFERRED_NODE_AUTO) {
        return Some(NumaPinPreference::Auto);
    }

    let node = trimmed.parse::<i32>().ok()?;
    (node >= 0).then_some(NumaPinPreference::Node(node))
}

/// Coarse fallback strategy the loader took when the NUMA optimization could
/// not be applied. The variants are designed so an operator inspecting
/// `ee status --json` can tell at a glance why a snapshot is not pinned.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NumaPinFallbackPath {
    /// No fallback was taken — pinning succeeded.
    None,
    /// Linux scaffold path that intentionally does not call mbind yet; the
    /// syscall implementation is tracked under bd-1prrl.3 follow-ups.
    SoftwareNotImplemented,
    /// macOS uses `madvise(MADV_WILLNEED)` + optional `mlock` as the closest
    /// available substitute. The scaffold does not invoke either yet; this
    /// variant records the *intended* fallback path so the wiring slice can
    /// adopt it without renaming the JSON enum.
    MadviseWillneed,
    /// Windows / other non-Linux platforms fall through to plain heap
    /// deserialization with no advice.
    HeapOnly,
    /// Operator explicitly disabled the optimization.
    DisabledByOperator,
}

/// Memory representation the graph snapshot loader should use for a platform.
/// This is a plan-level enum: the scaffold records intent without claiming the
/// syscall path has executed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NumaPinMappingKind {
    /// No mapping should be attempted because the optimization is disabled.
    None,
    /// Open the snapshot file read-only and mmap it directly.
    ReadOnlyMmap,
    /// Deserialize into ordinary process heap memory.
    HeapOnly,
}

impl NumaPinMappingKind {
    #[must_use]
    pub fn is_mmap(self) -> bool {
        matches!(self, Self::ReadOnlyMmap)
    }
}

/// Deterministic plan for mapping and pinning one graph snapshot. The plan is
/// intentionally side-effect-free: it does not stat the path, mmap a file,
/// read host topology, or mutate scheduler state. That makes it safe to build
/// during graph snapshot selection without changing pack hashes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NumaPinPlan {
    pub platform: NumaPinPlatform,
    pub supported: bool,
    pub enabled: bool,
    pub mapping_kind: NumaPinMappingKind,
    pub bind_requested: bool,
    pub preferred_node: Cow<'static, str>,
    pub populate_requested: bool,
    pub snapshot_bytes: u64,
    pub snapshot_path: PathBuf,
    pub fallback_path: NumaPinFallbackPath,
    pub degraded_codes: Vec<&'static str>,
}

impl NumaPinPlan {
    fn for_platform(
        platform: NumaPinPlatform,
        snapshot_path: &Path,
        snapshot_bytes: u64,
        config: &NumaPinConfig,
    ) -> Self {
        let mut plan = Self {
            platform,
            supported: platform.is_supported(),
            enabled: config.enabled,
            mapping_kind: NumaPinMappingKind::None,
            bind_requested: false,
            preferred_node: config.preferred_node.as_str(),
            populate_requested: config.populate_on_load,
            snapshot_bytes,
            snapshot_path: snapshot_path.to_path_buf(),
            fallback_path: NumaPinFallbackPath::None,
            degraded_codes: Vec::new(),
        };

        if !config.enabled {
            plan.fallback_path = NumaPinFallbackPath::DisabledByOperator;
            plan.push_unique_code(NUMA_PIN_DISABLED_CODE);
            return plan;
        }

        match platform {
            NumaPinPlatform::Linux => {
                plan.mapping_kind = NumaPinMappingKind::ReadOnlyMmap;
                plan.bind_requested = true;
                plan.fallback_path = NumaPinFallbackPath::SoftwareNotImplemented;
                plan.push_unique_code(NUMA_PIN_LINUX_NOT_IMPLEMENTED_CODE);
            }
            NumaPinPlatform::MacosUnsupported => {
                plan.mapping_kind = NumaPinMappingKind::ReadOnlyMmap;
                plan.fallback_path = NumaPinFallbackPath::MadviseWillneed;
                plan.push_unique_code(NUMA_PIN_UNSUPPORTED_PLATFORM_CODE);
            }
            NumaPinPlatform::WindowsUnsupported | NumaPinPlatform::OtherUnsupported => {
                plan.mapping_kind = NumaPinMappingKind::HeapOnly;
                plan.fallback_path = NumaPinFallbackPath::HeapOnly;
                plan.push_unique_code(NUMA_PIN_UNSUPPORTED_PLATFORM_CODE);
            }
        }

        plan
    }

    fn push_unique_code(&mut self, code: &'static str) {
        if !self.degraded_codes.contains(&code) {
            self.degraded_codes.push(code);
        }
    }
}

/// Input for building the deterministic per-snapshot NUMA hint row. This is
/// the graph-layer contract the database side-table can persist without
/// learning any platform-specific syscall details.
#[derive(Clone, Copy, Debug)]
pub struct GraphSnapshotNumaHintInput<'a> {
    pub snapshot_id: &'a str,
    pub graph_type: &'a str,
    pub snapshot_version: u64,
    pub source_generation: u32,
    pub content_hash: &'a str,
    pub snapshot_path: &'a Path,
    pub snapshot_bytes: u64,
    pub config: NumaPinConfig,
}

/// Deterministic side-table row describing the NUMA posture requested for one
/// graph snapshot generation. It records the file path, requested node, and
/// MAP_POPULATE posture without mmap'ing or binding pages itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSnapshotNumaHintRecord {
    pub schema: &'static str,
    pub snapshot_id: String,
    pub graph_type: String,
    pub snapshot_version: u64,
    pub source_generation: u32,
    pub content_hash: String,
    pub snapshot_path: PathBuf,
    pub snapshot_bytes: u64,
    pub requested_node: Cow<'static, str>,
    pub map_populate_requested: bool,
    pub mapping_kind: NumaPinMappingKind,
    pub bind_requested: bool,
    pub fallback_path: NumaPinFallbackPath,
    pub degraded_codes: Vec<&'static str>,
}

/// Build the side-table hint row for one graph snapshot. The function is
/// side-effect-free and derives every field from persisted snapshot metadata
/// plus [`NumaPinConfig`], so it cannot perturb graph determinism.
#[must_use]
pub fn graph_snapshot_numa_hint(
    input: GraphSnapshotNumaHintInput<'_>,
) -> GraphSnapshotNumaHintRecord {
    let plan = plan_snapshot_pin(input.snapshot_path, input.snapshot_bytes, &input.config);
    GraphSnapshotNumaHintRecord {
        schema: GRAPH_SNAPSHOT_NUMA_HINT_SCHEMA_V1,
        snapshot_id: input.snapshot_id.to_owned(),
        graph_type: input.graph_type.to_owned(),
        snapshot_version: input.snapshot_version,
        source_generation: input.source_generation,
        content_hash: input.content_hash.to_owned(),
        snapshot_path: input.snapshot_path.to_path_buf(),
        snapshot_bytes: input.snapshot_bytes,
        requested_node: plan.preferred_node,
        map_populate_requested: plan.populate_requested,
        mapping_kind: plan.mapping_kind,
        bind_requested: plan.bind_requested,
        fallback_path: plan.fallback_path,
        degraded_codes: plan.degraded_codes,
    }
}

/// Outcome of attempting to pin a snapshot blob. The shape is intentionally
/// flat-and-Serialize so the wiring slice can drop it straight into the
/// `data.graph.numaPin` block of `ee status --json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NumaPinResult {
    pub schema: &'static str,
    pub platform: NumaPinPlatform,
    pub supported: bool,
    pub enabled: bool,
    pub attempted: bool,
    pub succeeded: bool,
    pub preferred_node: Cow<'static, str>,
    pub populate_requested: bool,
    pub bytes_resident: u64,
    pub populated: bool,
    pub fallback_path: NumaPinFallbackPath,
    pub snapshot_path: Option<PathBuf>,
    pub degraded_codes: Vec<&'static str>,
}

impl NumaPinResult {
    fn base(platform: NumaPinPlatform, config: &NumaPinConfig, snapshot_path: &Path) -> Self {
        Self {
            schema: STATUS_GRAPH_NUMA_PIN_SCHEMA_V1,
            platform,
            supported: platform.is_supported(),
            enabled: config.enabled,
            attempted: false,
            succeeded: false,
            preferred_node: config.preferred_node.as_str(),
            populate_requested: config.populate_on_load,
            bytes_resident: 0,
            populated: false,
            fallback_path: NumaPinFallbackPath::None,
            snapshot_path: Some(snapshot_path.to_path_buf()),
            degraded_codes: Vec::new(),
        }
    }
}

/// Probe for the NUMA node the calling thread is currently scheduled on.
/// The scaffold returns `None` on every platform; the Linux wiring slice
/// (tracked under bd-1prrl.3) will replace this with a real `sched_getcpu`
/// + `numa_node_of_cpu` lookup and the host-calibration probe
///   (bd-1zb7k.12) once that bead lands.
#[must_use]
pub fn detect_preferred_node() -> Option<i32> {
    None
}

/// Return the coarse NUMA support classification for the running host.
#[must_use]
pub fn platform_support() -> NumaPinPlatform {
    NumaPinPlatform::detect()
}

/// Build the side-effect-free plan for mapping and pinning a graph snapshot.
/// Callers pass `snapshot_bytes` from already-known snapshot metadata; this
/// function deliberately avoids filesystem metadata reads so status/context
/// planning stays deterministic and cheap.
#[must_use]
pub fn plan_snapshot_pin(
    snapshot_path: &Path,
    snapshot_bytes: u64,
    config: &NumaPinConfig,
) -> NumaPinPlan {
    NumaPinPlan::for_platform(
        NumaPinPlatform::detect(),
        snapshot_path,
        snapshot_bytes,
        config,
    )
}

/// Attempt to pin a serialized graph snapshot blob to the NUMA node
/// indicated by `config`. The scaffold never panics, never mutates the
/// filesystem, and never claims a snapshot was pinned that wasn't — every
/// non-success path populates `degraded_codes` with a code documented in
/// `tests/fixtures/failure_modes/`.
pub fn pin_snapshot_blob(snapshot_path: &Path, config: &NumaPinConfig) -> NumaPinResult {
    let plan = plan_snapshot_pin(snapshot_path, 0, config);
    let mut result = NumaPinResult::base(plan.platform, config, snapshot_path);

    if !config.enabled {
        result.fallback_path = plan.fallback_path;
        result.degraded_codes = plan.degraded_codes;
        return result;
    }

    match plan.platform {
        NumaPinPlatform::Linux => {
            result.attempted = plan.bind_requested;
            result.fallback_path = plan.fallback_path;
            result.degraded_codes = plan.degraded_codes;
            result
        }
        NumaPinPlatform::MacosUnsupported => {
            result.fallback_path = plan.fallback_path;
            result.degraded_codes = plan.degraded_codes;
            result
        }
        NumaPinPlatform::WindowsUnsupported | NumaPinPlatform::OtherUnsupported => {
            result.fallback_path = plan.fallback_path;
            result.degraded_codes = plan.degraded_codes;
            result
        }
    }
}

// ---------------------------------------------------------------------------
// NumaPinningAdapter — bd-1prrl.3 (swarmx.4 trait abstraction)
//
// The trait isolates the platform-specific bits (NUMA-node affinity, mmap
// pinning) behind a single adapter so the snapshot-load path stays uniform
// across Linux + macOS while still emitting honest per-platform degraded
// codes. The bead body deliberately allows the `mmap` portion to ship on
// macOS as a portable best-effort path, while the NUMA hooks stay
// Linux-only — Mac returns `numa_unavailable_on_macos` from
// `set_node_affinity` and the snapshot loader keeps running without NUMA
// guidance.
//
// `#![forbid(unsafe_code)]` is intact at the crate level. The real libnuma
// FFI must land behind a safe adapter dependency (memmap2 + a safe
// numa-lib wrapper) in a follow-up slice; this slice ships the trait
// shape, both impls, the factory, and Mac-runnable tests so downstream
// wiring can target the trait now instead of the concrete function.
// ---------------------------------------------------------------------------

/// Degraded code emitted by `MacosNumaPinningAdapter::set_node_affinity` so
/// `ee status --json` / `ee doctor --json` can surface the platform-honest
/// "NUMA is a Linux-only concept here" message without claiming a fallback
/// the kernel cannot deliver. Pairs with the existing
/// `numa_pin_unsupported_platform` (which describes the platform as a
/// whole); the new code names the specific affinity-set operation, so
/// downstream parsers can react to the syscall-shaped gap separately from
/// the umbrella unavailability.
pub const NUMA_UNAVAILABLE_ON_MACOS_CODE: &str = "numa_unavailable_on_macos";

/// Outcome of a single adapter operation. Shape is intentionally tiny so
/// callers can fold it into the existing `NumaPinResult` envelope without
/// introducing a second schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NumaPinningAdapterOutcome {
    /// `true` when the operation actually executed against the platform
    /// (Linux + NUMA available). `false` when the adapter fell through
    /// to a degraded no-op.
    pub executed: bool,
    /// Stable code naming the degraded path, if any. `None` on a
    /// successful platform-native execution.
    pub degraded_code: Option<&'static str>,
}

impl NumaPinningAdapterOutcome {
    #[must_use]
    pub const fn executed() -> Self {
        Self {
            executed: true,
            degraded_code: None,
        }
    }

    #[must_use]
    pub const fn degraded(code: &'static str) -> Self {
        Self {
            executed: false,
            degraded_code: Some(code),
        }
    }
}

/// Adapter that hides the platform-specific NUMA + mmap calls from the
/// snapshot-load path. Two ops:
///
/// - `pin_mmap` — request that the snapshot blob be mapped with
///   pre-faulting and (on Linux) hugepage advice. The portable
///   memmap2-backed implementation is acceptable on macOS as a
///   best-effort path; the Linux impl additionally requests hugepages
///   when the operator enables them.
/// - `set_node_affinity` — bind the calling thread to the requested
///   NUMA node so subsequent allocations land on the local memory
///   controller. Linux-only; macOS returns
///   `NUMA_UNAVAILABLE_ON_MACOS_CODE`.
///
/// The trait deliberately does not return a `Mapping` handle yet. The
/// scaffold's `pin_snapshot_blob` keeps owning the result envelope; the
/// adapter only reports whether each operation actually ran. The follow-up
/// slice that adds memmap2 will introduce a handle-returning variant.
pub trait NumaPinningAdapter: Send + Sync {
    /// Coarse platform label for this adapter (used to populate the
    /// `platform` field of `NumaPinResult` consistently with the existing
    /// `platform_support()` detector).
    fn platform(&self) -> NumaPinPlatform;

    /// Request mmap pinning of the snapshot blob at `snapshot_path`.
    /// `populate` mirrors `NumaPinConfig::populate_on_load`. The default
    /// implementation here is a deterministic no-op so the scaffold
    /// remains test-runnable on every target; concrete impls override
    /// with the real syscall path when their platform allows.
    fn pin_mmap(&self, snapshot_path: &Path, populate: bool) -> NumaPinningAdapterOutcome {
        let _ = (snapshot_path, populate);
        NumaPinningAdapterOutcome::degraded(NUMA_PIN_LINUX_NOT_IMPLEMENTED_CODE)
    }

    /// Bind the calling thread to the requested NUMA node. `node` is the
    /// resolved Linux NUMA node id (or `None` for "let the kernel pick").
    fn set_node_affinity(&self, node: Option<i32>) -> NumaPinningAdapterOutcome;
}

/// macOS adapter. macOS has no NUMA primitives; the snapshot loader can
/// still benefit from the portable mmap path (mlock + MADV_WILLNEED via
/// memmap2 in the follow-up slice), so `pin_mmap` is a best-effort
/// no-op-then-degraded today and the affinity op explicitly emits the
/// new `numa_unavailable_on_macos` code.
#[derive(Clone, Copy, Debug, Default)]
pub struct MacosNumaPinningAdapter;

impl NumaPinningAdapter for MacosNumaPinningAdapter {
    fn platform(&self) -> NumaPinPlatform {
        NumaPinPlatform::MacosUnsupported
    }

    fn pin_mmap(&self, _snapshot_path: &Path, _populate: bool) -> NumaPinningAdapterOutcome {
        // The portable mmap path is acceptable on macOS, but the scaffold
        // ships before the memmap2 safe-wrapper dep lands. Honestly mark
        // the op as not-executed under the umbrella platform code so
        // status output stays truthful.
        NumaPinningAdapterOutcome::degraded(NUMA_PIN_UNSUPPORTED_PLATFORM_CODE)
    }

    fn set_node_affinity(&self, _node: Option<i32>) -> NumaPinningAdapterOutcome {
        // macOS has no NUMA primitives. This is the load-bearing degraded
        // code the bead body names: every macOS host reports the
        // operation as deliberately unavailable rather than silently
        // succeeding.
        NumaPinningAdapterOutcome::degraded(NUMA_UNAVAILABLE_ON_MACOS_CODE)
    }
}

/// Linux adapter. The trait shape ships now; the libnuma + mmap syscall
/// payload lands in the follow-up slice once a safe-wrapper dep
/// (memmap2 + safe-numa) is approved. Until then both ops emit
/// `numa_pin_linux_not_implemented` so the production path keeps the
/// honest scaffold behavior already pinned by the existing tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxNumaPinningAdapter;

impl NumaPinningAdapter for LinuxNumaPinningAdapter {
    fn platform(&self) -> NumaPinPlatform {
        NumaPinPlatform::Linux
    }

    #[cfg(target_os = "linux")]
    fn pin_mmap(&self, _snapshot_path: &Path, _populate: bool) -> NumaPinningAdapterOutcome {
        NumaPinningAdapterOutcome::degraded(NUMA_PIN_LINUX_NOT_IMPLEMENTED_CODE)
    }

    #[cfg(not(target_os = "linux"))]
    fn pin_mmap(&self, _snapshot_path: &Path, _populate: bool) -> NumaPinningAdapterOutcome {
        // The Linux adapter constructed on a non-Linux build is a
        // pure-architecture object (used by tests that exercise the
        // trait dispatch shape). Surface the umbrella platform code
        // rather than pretending the Linux path executed.
        NumaPinningAdapterOutcome::degraded(NUMA_PIN_UNSUPPORTED_PLATFORM_CODE)
    }

    #[cfg(target_os = "linux")]
    fn set_node_affinity(&self, _node: Option<i32>) -> NumaPinningAdapterOutcome {
        NumaPinningAdapterOutcome::degraded(NUMA_PIN_LINUX_NOT_IMPLEMENTED_CODE)
    }

    #[cfg(not(target_os = "linux"))]
    fn set_node_affinity(&self, _node: Option<i32>) -> NumaPinningAdapterOutcome {
        NumaPinningAdapterOutcome::degraded(NUMA_PIN_UNSUPPORTED_PLATFORM_CODE)
    }
}

/// Returns the adapter for the running host. Linux returns
/// `LinuxNumaPinningAdapter`; every other platform returns
/// `MacosNumaPinningAdapter` (the only non-Linux concrete impl, which
/// happens to map cleanly to BSD/Windows-Unsupported in this scaffold —
/// the umbrella code distinguishes the cases for now).
#[must_use]
pub fn default_numa_pinning_adapter() -> &'static dyn NumaPinningAdapter {
    #[cfg(target_os = "linux")]
    static LINUX_ADAPTER: LinuxNumaPinningAdapter = LinuxNumaPinningAdapter;
    #[cfg(not(target_os = "linux"))]
    static NON_LINUX_ADAPTER: MacosNumaPinningAdapter = MacosNumaPinningAdapter;

    #[cfg(target_os = "linux")]
    {
        &LINUX_ADAPTER
    }
    #[cfg(not(target_os = "linux"))]
    {
        &NON_LINUX_ADAPTER
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::Path;

    #[cfg(not(target_os = "linux"))]
    use super::NUMA_PIN_UNSUPPORTED_PLATFORM_CODE;
    use super::{
        GRAPH_SNAPSHOT_NUMA_HINT_SCHEMA_V1, GraphSnapshotNumaHintInput, NUMA_PIN_DISABLE_ENV,
        NUMA_PIN_DISABLED_CODE, NUMA_PIN_NODE_ENV, NUMA_PIN_POPULATE_ENV,
        NUMA_PIN_PREFERRED_NODE_AUTO, NumaPinConfig, NumaPinFallbackPath, NumaPinMappingKind,
        NumaPinPlan, NumaPinPlatform, NumaPinPreference, NumaPinResult,
        STATUS_GRAPH_NUMA_PIN_SCHEMA_V1, detect_preferred_node, graph_snapshot_numa_hint,
        parse_env_bool, parse_env_preferred_node, pin_snapshot_blob, plan_snapshot_pin,
        platform_support,
    };

    fn fake_snapshot_path() -> &'static Path {
        Path::new("/tmp/ee-numa-pin-fake-snapshot.bin")
    }

    fn env_reader_from<'a>(
        entries: &'a [(&'static str, &'static str)],
    ) -> impl Fn(&'static str) -> Option<String> + 'a {
        let map: HashMap<&'static str, &'static str> = entries.iter().copied().collect();
        move |name: &'static str| map.get(name).map(|value| (*value).to_owned())
    }

    fn assert_no_duplicate_codes(result: &NumaPinResult) {
        let mut seen = std::collections::BTreeSet::new();
        for code in &result.degraded_codes {
            assert!(
                seen.insert(*code),
                "duplicate degraded code {code} in {:?}",
                result.degraded_codes
            );
        }
    }

    fn assert_plan_has_no_duplicate_codes(plan: &NumaPinPlan) {
        let mut seen = std::collections::BTreeSet::new();
        for code in &plan.degraded_codes {
            assert!(
                seen.insert(*code),
                "duplicate degraded code {code} in {:?}",
                plan.degraded_codes
            );
        }
    }

    #[test]
    fn default_config_is_enabled_and_auto() {
        let config = NumaPinConfig::default();
        assert!(config.enabled);
        assert_eq!(config.preferred_node, NumaPinPreference::Auto);
        assert!(config.populate_on_load);
    }

    #[test]
    fn disabled_config_short_circuits_with_disabled_code() {
        let result = pin_snapshot_blob(fake_snapshot_path(), &NumaPinConfig::disabled());
        assert!(!result.enabled);
        assert!(!result.attempted);
        assert!(!result.succeeded);
        assert_eq!(
            result.fallback_path,
            NumaPinFallbackPath::DisabledByOperator
        );
        assert_eq!(result.degraded_codes, vec![NUMA_PIN_DISABLED_CODE]);
        assert_no_duplicate_codes(&result);
    }

    #[test]
    fn disabled_pin_plan_never_requests_mapping_or_bind() {
        let plan = plan_snapshot_pin(fake_snapshot_path(), 4096, &NumaPinConfig::disabled());
        assert_eq!(plan.mapping_kind, NumaPinMappingKind::None);
        assert!(!plan.mapping_kind.is_mmap());
        assert!(!plan.bind_requested);
        assert_eq!(plan.snapshot_bytes, 4096);
        assert_eq!(plan.fallback_path, NumaPinFallbackPath::DisabledByOperator);
        assert_eq!(plan.degraded_codes, vec![NUMA_PIN_DISABLED_CODE]);
        assert_plan_has_no_duplicate_codes(&plan);
    }

    #[test]
    fn preferred_node_renders_auto_and_explicit_consistently() {
        assert_eq!(
            NumaPinPreference::Auto.as_str(),
            NUMA_PIN_PREFERRED_NODE_AUTO
        );
        assert_eq!(NumaPinPreference::Node(0).as_str(), "0");
        assert_eq!(NumaPinPreference::Node(7).as_str(), "7");
    }

    #[test]
    fn parse_env_bool_accepts_operator_vocabulary() {
        for raw in ["true", "TRUE", "1", "yes", "YES", "on", " ON "] {
            assert_eq!(parse_env_bool(raw), Some(true));
        }
        for raw in ["false", "FALSE", "0", "no", "NO", "off", " OFF "] {
            assert_eq!(parse_env_bool(raw), Some(false));
        }
        for raw in ["maybe", "2", "", "  "] {
            assert_eq!(parse_env_bool(raw), None);
        }
    }

    #[test]
    fn parse_env_preferred_node_accepts_auto_and_non_negative_nodes() {
        assert_eq!(
            parse_env_preferred_node("auto"),
            Some(NumaPinPreference::Auto)
        );
        assert_eq!(
            parse_env_preferred_node(" AUTO "),
            Some(NumaPinPreference::Auto)
        );
        assert_eq!(
            parse_env_preferred_node("0"),
            Some(NumaPinPreference::Node(0))
        );
        assert_eq!(
            parse_env_preferred_node("7"),
            Some(NumaPinPreference::Node(7))
        );
        assert_eq!(parse_env_preferred_node("-1"), None);
        assert_eq!(parse_env_preferred_node("socket0"), None);
    }

    #[test]
    fn from_environment_with_empty_reader_yields_default_config() {
        let unparseable: RefCell<Vec<(&'static str, String)>> = RefCell::new(Vec::new());
        let config = NumaPinConfig::from_environment_with_reader(
            |_name| None,
            |name, raw| unparseable.borrow_mut().push((name, raw.to_owned())),
        );
        assert_eq!(config, NumaPinConfig::default());
        assert!(
            unparseable.borrow().is_empty(),
            "missing values must not trigger on_unparseable: {:?}",
            unparseable.borrow()
        );
    }

    #[test]
    fn from_environment_parses_disable_node_and_populate_controls() {
        let unparseable: RefCell<Vec<(&'static str, String)>> = RefCell::new(Vec::new());
        let reader = env_reader_from(&[
            (NUMA_PIN_DISABLE_ENV, "yes"),
            (NUMA_PIN_NODE_ENV, "2"),
            (NUMA_PIN_POPULATE_ENV, "false"),
        ]);
        let config = NumaPinConfig::from_environment_with_reader(reader, |name, raw| {
            unparseable.borrow_mut().push((name, raw.to_owned()))
        });
        assert!(!config.enabled);
        assert_eq!(config.preferred_node, NumaPinPreference::Node(2));
        assert!(!config.populate_on_load);
        assert!(unparseable.borrow().is_empty());
    }

    #[test]
    fn from_environment_records_unparseable_values_without_changing_defaults() {
        let unparseable: RefCell<Vec<(&'static str, String)>> = RefCell::new(Vec::new());
        let reader = env_reader_from(&[
            (NUMA_PIN_DISABLE_ENV, "maybe"),
            (NUMA_PIN_NODE_ENV, "-4"),
            (NUMA_PIN_POPULATE_ENV, "sometimes"),
        ]);
        let config = NumaPinConfig::from_environment_with_reader(reader, |name, raw| {
            unparseable.borrow_mut().push((name, raw.to_owned()))
        });
        assert_eq!(config, NumaPinConfig::default());
        let log = unparseable.borrow();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0], (NUMA_PIN_DISABLE_ENV, "maybe".to_string()));
        assert_eq!(log[1], (NUMA_PIN_NODE_ENV, "-4".to_string()));
        assert_eq!(log[2], (NUMA_PIN_POPULATE_ENV, "sometimes".to_string()));
    }

    #[test]
    fn from_environment_constants_match_swarmx4_spec() {
        assert_eq!(NUMA_PIN_DISABLE_ENV, "EE_GRAPH_NUMA_PIN_DISABLE");
        assert_eq!(NUMA_PIN_NODE_ENV, "EE_GRAPH_NUMA_PIN_NODE");
        assert_eq!(NUMA_PIN_POPULATE_ENV, "EE_GRAPH_NUMA_PIN_POPULATE");
    }

    #[test]
    fn detect_preferred_node_returns_none_on_scaffold() {
        assert_eq!(detect_preferred_node(), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_pin_plan_requests_readonly_mmap_and_bind_without_claiming_success() {
        let plan = plan_snapshot_pin(
            fake_snapshot_path(),
            128 * 1024 * 1024,
            &NumaPinConfig::default().with_preferred_node(NumaPinPreference::Node(1)),
        );
        assert_eq!(plan.platform, NumaPinPlatform::Linux);
        assert!(plan.supported);
        assert_eq!(plan.mapping_kind, NumaPinMappingKind::ReadOnlyMmap);
        assert!(plan.mapping_kind.is_mmap());
        assert!(plan.bind_requested);
        assert_eq!(plan.preferred_node, "1");
        assert_eq!(plan.snapshot_bytes, 128 * 1024 * 1024);
        assert_eq!(
            plan.fallback_path,
            NumaPinFallbackPath::SoftwareNotImplemented
        );
        assert_eq!(
            plan.degraded_codes,
            vec![NUMA_PIN_LINUX_NOT_IMPLEMENTED_CODE]
        );
        assert_plan_has_no_duplicate_codes(&plan);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_pin_plan_is_deterministic_and_non_binding() {
        let plan = plan_snapshot_pin(
            fake_snapshot_path(),
            128 * 1024 * 1024,
            &NumaPinConfig::default().with_preferred_node(NumaPinPreference::Node(1)),
        );
        assert!(!plan.supported);
        assert!(!plan.bind_requested);
        assert_eq!(plan.preferred_node, "1");
        assert_eq!(plan.snapshot_bytes, 128 * 1024 * 1024);
        assert!(
            plan.degraded_codes
                .iter()
                .any(|code| code == NUMA_PIN_UNSUPPORTED_PLATFORM_CODE)
        );
        match plan.platform {
            NumaPinPlatform::MacosUnsupported => {
                assert_eq!(plan.mapping_kind, NumaPinMappingKind::ReadOnlyMmap);
                assert_eq!(plan.fallback_path, NumaPinFallbackPath::MadviseWillneed);
            }
            NumaPinPlatform::WindowsUnsupported | NumaPinPlatform::OtherUnsupported => {
                assert_eq!(plan.mapping_kind, NumaPinMappingKind::HeapOnly);
                assert_eq!(plan.fallback_path, NumaPinFallbackPath::HeapOnly);
            }
            NumaPinPlatform::Linux => panic!("linux cfg should run the linux-specific plan test"),
        }
        assert_plan_has_no_duplicate_codes(&plan);
    }

    #[test]
    fn platform_support_is_consistent_with_cfg() {
        let platform = platform_support();
        if cfg!(target_os = "linux") {
            assert_eq!(platform, NumaPinPlatform::Linux);
            assert!(platform.is_supported());
        } else if cfg!(target_os = "macos") {
            assert_eq!(platform, NumaPinPlatform::MacosUnsupported);
            assert!(!platform.is_supported());
        } else if cfg!(target_os = "windows") {
            assert_eq!(platform, NumaPinPlatform::WindowsUnsupported);
            assert!(!platform.is_supported());
        } else {
            assert_eq!(platform, NumaPinPlatform::OtherUnsupported);
            assert!(!platform.is_supported());
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_platform_returns_unsupported_code() {
        let result = pin_snapshot_blob(fake_snapshot_path(), &NumaPinConfig::default());
        assert!(!result.supported);
        assert!(result.enabled);
        assert!(!result.attempted);
        assert!(!result.succeeded);
        assert_eq!(result.bytes_resident, 0);
        assert!(!result.populated);
        assert!(matches!(
            result.fallback_path,
            NumaPinFallbackPath::MadviseWillneed
                | NumaPinFallbackPath::HeapOnly
                | NumaPinFallbackPath::DisabledByOperator
        ));
        assert!(
            result
                .degraded_codes
                .iter()
                .any(|code| code == NUMA_PIN_UNSUPPORTED_PLATFORM_CODE)
        );
        assert_no_duplicate_codes(&result);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_scaffold_reports_not_implemented_without_claiming_success() {
        let result = pin_snapshot_blob(fake_snapshot_path(), &NumaPinConfig::default());
        assert_eq!(result.platform, NumaPinPlatform::Linux);
        assert!(result.supported);
        assert!(result.enabled);
        assert!(result.attempted);
        assert!(!result.succeeded, "scaffold must not claim success");
        assert!(!result.populated, "scaffold must not claim populated pages");
        assert_eq!(
            result.fallback_path,
            NumaPinFallbackPath::SoftwareNotImplemented
        );
        assert!(
            result
                .degraded_codes
                .iter()
                .any(|code| code == NUMA_PIN_LINUX_NOT_IMPLEMENTED_CODE)
        );
        assert_no_duplicate_codes(&result);
    }

    #[test]
    fn result_schema_matches_documented_id() {
        let result = pin_snapshot_blob(fake_snapshot_path(), &NumaPinConfig::default());
        assert_eq!(result.schema, STATUS_GRAPH_NUMA_PIN_SCHEMA_V1);
        assert_eq!(
            STATUS_GRAPH_NUMA_PIN_SCHEMA_V1,
            "ee.status.graph.numa_pin.v1"
        );
    }

    #[test]
    fn config_builder_methods_round_trip() {
        let config = NumaPinConfig::default()
            .with_preferred_node(NumaPinPreference::Node(3))
            .with_populate_on_load(false);
        assert_eq!(config.preferred_node, NumaPinPreference::Node(3));
        assert!(!config.populate_on_load);
        assert!(config.enabled);
    }

    #[test]
    fn graph_snapshot_numa_hint_records_side_table_fields_from_plan() {
        let hint = graph_snapshot_numa_hint(GraphSnapshotNumaHintInput {
            snapshot_id: "snap-01",
            graph_type: "memory_links",
            snapshot_version: 17,
            source_generation: 42,
            content_hash: "blake3:abc123",
            snapshot_path: Path::new("/var/lib/ee/snapshots/graph.bin"),
            snapshot_bytes: 128 * 1024 * 1024,
            config: NumaPinConfig::default().with_preferred_node(NumaPinPreference::Node(1)),
        });

        assert_eq!(hint.schema, GRAPH_SNAPSHOT_NUMA_HINT_SCHEMA_V1);
        assert_eq!(hint.snapshot_id, "snap-01");
        assert_eq!(hint.graph_type, "memory_links");
        assert_eq!(hint.snapshot_version, 17);
        assert_eq!(hint.source_generation, 42);
        assert_eq!(hint.content_hash, "blake3:abc123");
        assert_eq!(
            hint.snapshot_path.as_path(),
            Path::new("/var/lib/ee/snapshots/graph.bin")
        );
        assert_eq!(hint.snapshot_bytes, 128 * 1024 * 1024);
        assert_eq!(hint.requested_node, "1");
        assert!(hint.map_populate_requested);
        if cfg!(target_os = "linux") {
            assert_eq!(hint.mapping_kind, NumaPinMappingKind::ReadOnlyMmap);
            assert!(hint.bind_requested);
        } else {
            assert!(!hint.bind_requested);
        }
    }

    #[test]
    fn disabled_graph_snapshot_numa_hint_never_requests_mapping() {
        let hint = graph_snapshot_numa_hint(GraphSnapshotNumaHintInput {
            snapshot_id: "snap-disabled",
            graph_type: "revision_dag",
            snapshot_version: 1,
            source_generation: 3,
            content_hash: "blake3:def456",
            snapshot_path: fake_snapshot_path(),
            snapshot_bytes: 4096,
            config: NumaPinConfig::disabled(),
        });

        assert_eq!(hint.schema, GRAPH_SNAPSHOT_NUMA_HINT_SCHEMA_V1);
        assert_eq!(hint.mapping_kind, NumaPinMappingKind::None);
        assert!(!hint.bind_requested);
        assert_eq!(hint.fallback_path, NumaPinFallbackPath::DisabledByOperator);
        assert_eq!(hint.degraded_codes, vec![NUMA_PIN_DISABLED_CODE]);
    }

    #[test]
    fn graph_snapshot_numa_hint_serializes_camel_case_fields() {
        let hint = graph_snapshot_numa_hint(GraphSnapshotNumaHintInput {
            snapshot_id: "snap-json",
            graph_type: "causal_evidence",
            snapshot_version: 5,
            source_generation: 8,
            content_hash: "blake3:json",
            snapshot_path: fake_snapshot_path(),
            snapshot_bytes: 8192,
            config: NumaPinConfig::disabled(),
        });
        let serialized = serde_json::to_value(&hint).expect("serialize hint");
        for key in [
            "schema",
            "snapshotId",
            "graphType",
            "snapshotVersion",
            "sourceGeneration",
            "contentHash",
            "snapshotPath",
            "snapshotBytes",
            "requestedNode",
            "mapPopulateRequested",
            "mappingKind",
            "bindRequested",
            "fallbackPath",
            "degradedCodes",
        ] {
            assert!(
                serialized.get(key).is_some(),
                "expected field {key} in serialized hint {serialized}"
            );
        }
        assert_eq!(
            serialized.get("schema").and_then(|value| value.as_str()),
            Some(GRAPH_SNAPSHOT_NUMA_HINT_SCHEMA_V1)
        );
        assert_eq!(
            serialized
                .get("fallbackPath")
                .and_then(|value| value.as_str()),
            Some("disabled_by_operator")
        );
    }

    #[test]
    fn pin_snapshot_blob_preserves_snapshot_path_in_result() {
        let path = Path::new("/var/lib/ee/snapshots/example.bin");
        let result = pin_snapshot_blob(path, &NumaPinConfig::default());
        assert_eq!(result.snapshot_path.as_deref(), Some(path));
    }

    #[test]
    fn result_serializes_with_camel_case_fields() {
        let result = pin_snapshot_blob(fake_snapshot_path(), &NumaPinConfig::disabled());
        let serialized = serde_json::to_value(&result).expect("serialize result");
        for key in [
            "schema",
            "platform",
            "supported",
            "enabled",
            "attempted",
            "succeeded",
            "preferredNode",
            "populateRequested",
            "bytesResident",
            "populated",
            "fallbackPath",
            "snapshotPath",
            "degradedCodes",
        ] {
            assert!(
                serialized.get(key).is_some(),
                "expected field {key} in serialized result {serialized}"
            );
        }
        assert_eq!(
            serialized
                .get("fallbackPath")
                .and_then(|value| value.as_str()),
            Some("disabled_by_operator")
        );
    }

    // bd-1prrl.3 — trait abstraction regression guards.
    //
    // Pin the contract that the trait shape is Mac-runnable and that the
    // load-bearing degraded codes flow through the adapter ops correctly.
    // These tests exercise the architecture surface, not the (still
    // owed) libnuma + memmap2 syscall payload — that lands in the
    // follow-up safe-adapter-dep slice once memmap2 is promoted to a
    // direct dependency.

    use super::{
        LinuxNumaPinningAdapter, MacosNumaPinningAdapter, NUMA_PIN_LINUX_NOT_IMPLEMENTED_CODE,
        NUMA_UNAVAILABLE_ON_MACOS_CODE, NumaPinningAdapter, NumaPinningAdapterOutcome,
        default_numa_pinning_adapter,
    };

    #[test]
    fn macos_adapter_set_node_affinity_emits_numa_unavailable_on_macos() {
        let adapter = MacosNumaPinningAdapter;
        let outcome = adapter.set_node_affinity(Some(0));
        assert!(!outcome.executed, "macOS adapter must not claim execution");
        assert_eq!(
            outcome.degraded_code,
            Some(NUMA_UNAVAILABLE_ON_MACOS_CODE),
            "macOS adapter must emit the load-bearing bd-1prrl.3 degraded code"
        );
        // `Auto` (None) must also degrade — the affinity-set op is
        // unavailable regardless of which node was requested.
        let auto_outcome = adapter.set_node_affinity(None);
        assert_eq!(
            auto_outcome.degraded_code,
            Some(NUMA_UNAVAILABLE_ON_MACOS_CODE),
            "macOS adapter must emit the same code for the auto-node request"
        );
    }

    #[test]
    fn macos_adapter_pin_mmap_falls_through_to_unsupported_platform() {
        // The portable mmap path will land with memmap2 in a follow-up
        // slice; until then macOS reports the umbrella platform code so
        // status output stays truthful and does not claim residency.
        let adapter = MacosNumaPinningAdapter;
        let outcome = adapter.pin_mmap(fake_snapshot_path(), true);
        assert!(!outcome.executed);
        assert_eq!(
            outcome.degraded_code,
            Some(super::NUMA_PIN_UNSUPPORTED_PLATFORM_CODE),
        );
        assert_eq!(adapter.platform(), NumaPinPlatform::MacosUnsupported);
    }

    #[test]
    fn linux_adapter_emits_not_implemented_until_safe_wrapper_lands() {
        // Constructed-on-non-Linux behavior: the adapter is a pure
        // architecture object (used by trait-dispatch tests) and falls
        // through to the umbrella platform code rather than pretending
        // the Linux path executed. On a real Linux build the same
        // adapter emits `numa_pin_linux_not_implemented` until the
        // libnuma + memmap2 syscall payload lands.
        let adapter = LinuxNumaPinningAdapter;
        assert_eq!(adapter.platform(), NumaPinPlatform::Linux);
        let outcome = adapter.pin_mmap(fake_snapshot_path(), true);
        let expected_code = if cfg!(target_os = "linux") {
            NUMA_PIN_LINUX_NOT_IMPLEMENTED_CODE
        } else {
            super::NUMA_PIN_UNSUPPORTED_PLATFORM_CODE
        };
        assert!(!outcome.executed);
        assert_eq!(outcome.degraded_code, Some(expected_code));
    }

    #[test]
    fn default_adapter_selects_platform_specific_implementation() {
        // The factory must return the Linux adapter on Linux and the
        // macOS adapter elsewhere. Tested via the `platform()` accessor
        // so the assertion stays Mac-runnable (no `is_a` reflection).
        let adapter = default_numa_pinning_adapter();
        let expected = if cfg!(target_os = "linux") {
            NumaPinPlatform::Linux
        } else if cfg!(target_os = "macos") {
            NumaPinPlatform::MacosUnsupported
        } else {
            // Other non-Linux platforms still receive the macOS-shaped
            // adapter under this scaffold; the umbrella platform code
            // distinguishes the cases at the result-envelope layer.
            NumaPinPlatform::MacosUnsupported
        };
        assert_eq!(adapter.platform(), expected);
    }

    #[test]
    fn outcome_helpers_round_trip_through_executed_and_degraded() {
        let executed = NumaPinningAdapterOutcome::executed();
        assert!(executed.executed);
        assert_eq!(executed.degraded_code, None);

        let degraded = NumaPinningAdapterOutcome::degraded(NUMA_UNAVAILABLE_ON_MACOS_CODE);
        assert!(!degraded.executed);
        assert_eq!(degraded.degraded_code, Some(NUMA_UNAVAILABLE_ON_MACOS_CODE));
    }

    #[test]
    fn trait_dispatch_through_box_dyn_compiles_and_runs_on_every_platform() {
        // Compile-time + runtime guard that the trait object stays
        // dyn-safe. Without `Send + Sync` bounds this assertion would
        // fail to compile; pin both bounds so future refactors cannot
        // silently break supervised threading.
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<dyn NumaPinningAdapter>();

        let adapter = default_numa_pinning_adapter();
        let outcome = adapter.set_node_affinity(Some(0));
        // The platform-specific code is whichever adapter we got; the
        // load-bearing invariant is just that *some* honest degraded
        // code surfaces — production never sees executed=true here
        // until the safe-wrapper slice lands.
        assert!(!outcome.executed);
        assert!(outcome.degraded_code.is_some());
    }
}
