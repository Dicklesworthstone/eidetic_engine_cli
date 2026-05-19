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
//! optimization: configuration types, a result envelope that records what
//! actually happened, the degraded-code vocabulary, and the entry points the
//! `refresh_graph_snapshot` / `load_graph_snapshot` consumers will eventually
//! call. The Linux libc::mbind real-syscall path and the wiring into
//! `src/graph/mod.rs` snapshot loaders are deferred to follow-up slices under
//! bd-1prrl.3.
//!
//! The scaffold is honest: on every non-Linux platform `pin_snapshot_blob`
//! returns a fully populated `NumaPinResult` with `supported=false`, a
//! fallback-path indicator, and the `numa_pin_unsupported_platform` degraded
//! code; on Linux it currently returns `succeeded=false` with the
//! `numa_pin_linux_not_implemented` degraded code so callers cannot mistake
//! the scaffold for a working syscall path. When the operator disables the
//! optimization via `NumaPinConfig::disabled()` the result short-circuits
//! with the `numa_pin_disabled` code regardless of platform.

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NumaPinPreference {
    Auto,
    Node(i32),
}

impl Default for NumaPinPreference {
    fn default() -> Self {
        Self::Auto
    }
}

impl NumaPinPreference {
    #[must_use]
    pub fn as_str(self) -> String {
        match self {
            Self::Auto => NUMA_PIN_PREFERRED_NODE_AUTO.to_string(),
            Self::Node(node) => node.to_string(),
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
    pub preferred_node: String,
    pub populate_requested: bool,
    pub bytes_resident: u64,
    pub populated: bool,
    pub fallback_path: NumaPinFallbackPath,
    pub snapshot_path: Option<PathBuf>,
    pub degraded_codes: Vec<String>,
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

    fn push_unique_code(&mut self, code: &str) {
        if !self.degraded_codes.iter().any(|existing| existing == code) {
            self.degraded_codes.push(code.to_string());
        }
    }
}

/// Probe for the NUMA node the calling thread is currently scheduled on.
/// The scaffold returns `None` on every platform; the Linux wiring slice
/// (tracked under bd-1prrl.3) will replace this with a real `sched_getcpu`
/// + `numa_node_of_cpu` lookup and the host-calibration probe
/// (bd-1zb7k.12) once that bead lands.
#[must_use]
pub fn detect_preferred_node() -> Option<i32> {
    None
}

/// Return the coarse NUMA support classification for the running host.
#[must_use]
pub fn platform_support() -> NumaPinPlatform {
    NumaPinPlatform::detect()
}

/// Attempt to pin a serialized graph snapshot blob to the NUMA node
/// indicated by `config`. The scaffold never panics, never mutates the
/// filesystem, and never claims a snapshot was pinned that wasn't — every
/// non-success path populates `degraded_codes` with a code documented in
/// `tests/fixtures/failure_modes/`.
pub fn pin_snapshot_blob(snapshot_path: &Path, config: &NumaPinConfig) -> NumaPinResult {
    let platform = NumaPinPlatform::detect();
    let mut result = NumaPinResult::base(platform, config, snapshot_path);

    if !config.enabled {
        result.fallback_path = NumaPinFallbackPath::DisabledByOperator;
        result.push_unique_code(NUMA_PIN_DISABLED_CODE);
        return result;
    }

    match platform {
        NumaPinPlatform::Linux => {
            result.attempted = true;
            result.fallback_path = NumaPinFallbackPath::SoftwareNotImplemented;
            result.push_unique_code(NUMA_PIN_LINUX_NOT_IMPLEMENTED_CODE);
            result
        }
        NumaPinPlatform::MacosUnsupported => {
            result.fallback_path = NumaPinFallbackPath::MadviseWillneed;
            result.push_unique_code(NUMA_PIN_UNSUPPORTED_PLATFORM_CODE);
            result
        }
        NumaPinPlatform::WindowsUnsupported | NumaPinPlatform::OtherUnsupported => {
            result.fallback_path = NumaPinFallbackPath::HeapOnly;
            result.push_unique_code(NUMA_PIN_UNSUPPORTED_PLATFORM_CODE);
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        NUMA_PIN_DISABLED_CODE, NUMA_PIN_LINUX_NOT_IMPLEMENTED_CODE, NUMA_PIN_PREFERRED_NODE_AUTO,
        NUMA_PIN_UNSUPPORTED_PLATFORM_CODE, NumaPinConfig, NumaPinFallbackPath, NumaPinPlatform,
        NumaPinPreference, NumaPinResult, STATUS_GRAPH_NUMA_PIN_SCHEMA_V1, detect_preferred_node,
        pin_snapshot_blob, platform_support,
    };

    fn fake_snapshot_path() -> &'static Path {
        Path::new("/tmp/ee-numa-pin-fake-snapshot.bin")
    }

    fn assert_no_duplicate_codes(result: &NumaPinResult) {
        let mut seen = std::collections::BTreeSet::new();
        for code in &result.degraded_codes {
            assert!(
                seen.insert(code.clone()),
                "duplicate degraded code {code} in {:?}",
                result.degraded_codes
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
        assert_eq!(
            result.degraded_codes,
            vec![NUMA_PIN_DISABLED_CODE.to_string()]
        );
        assert_no_duplicate_codes(&result);
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
    fn detect_preferred_node_returns_none_on_scaffold() {
        assert_eq!(detect_preferred_node(), None);
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
}
