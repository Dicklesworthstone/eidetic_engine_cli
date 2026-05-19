//! Lexical posting-list RAM-tier pinning — scaffold (bd-1hvzh, sub-bead of
//! bd-21xbi).
//!
//! On 256GB+ Linux hosts the Frankensearch lexical index files under
//! `indexes/combined/` are not RAM-tier pinned. The first cold search pays
//! disk page-fault cost on each access, and subsequent searches still
//! compete for page-cache slots with everything else on the host. bd-21xbi
//! adds an opt-in `mmap(MAP_POPULATE) + mlock` (optionally
//! `MADV_HUGEPAGE`) loader that pre-faults the lexical index into RAM and
//! holds it there. This is distinct from bd-1prrl.3 / swarmx.4 (graph
//! snapshot mmap), bd-ndzfg (L2 pack result cache), and bd-168gm (embedding
//! LRU) — each pins a different dataset.
//!
//! This scaffold owns the platform-agnostic public surface: configuration
//! types, the result envelope the wiring slice will surface under
//! `ee status --json` → `data.search.lexicalRamTier`, the degraded-code
//! vocabulary, and the entry point the index loader will eventually call.
//! The real Linux `mmap` + `MAP_POPULATE` + `mlock` + `MADV_HUGEPAGE`
//! syscall path, the wiring into `src/search/mod.rs`, the env-var registry
//! rows, and the `ee status` block emission live under sibling slices of
//! bd-21xbi so this module can land without touching any contested file.
//!
//! Determinism contract: the optimization only changes wall-clock and
//! page-cache residency; lexical search results MUST be byte-identical
//! whether the index is RAM-pinned or read from disk. The wiring slice
//! extends `tests/determinism_unit.rs` with the `pin_ram` × `request_hugepages`
//! dimensions.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// `degraded[]` code emitted when an operator requested transparent
/// hugepages but the host platform or kernel does not expose
/// `MADV_HUGEPAGE` (every non-Linux host, and Linux builds without THP
/// configured). Functionality is unchanged; the optimization falls back to
/// regular page-size mmap.
pub const LEXICAL_HUGEPAGES_UNAVAILABLE_CODE: &str = "lexical_hugepages_unavailable";

/// `degraded[]` code emitted when an operator has disabled the
/// optimization through `[search.lexical_ram_tier] enabled = false` (or
/// the env-var equivalent). Response-time classification per
/// `docs/degraded_code_taxonomy.md`.
pub const LEXICAL_RAM_TIER_DISABLED_CODE: &str = "lexical_ram_tier_disabled";

/// `degraded[]` code emitted while the scaffold ships without the real
/// `mmap` + `MAP_POPULATE` + `mlock` syscall path. Tracked under follow-up
/// slices of bd-21xbi; consumers MUST treat this exactly like the
/// hugepages-unavailable path (degrade gracefully, never panic, never
/// claim the index was actually pinned).
pub const LEXICAL_RAM_TIER_NOT_IMPLEMENTED_CODE: &str = "lexical_ram_tier_not_implemented";

/// Forward-looking schema id for the `ee status --json` lexicalRamTier
/// block, kept in sync with
/// `docs/schemas/ee.status.search.lexical_ram_tier.v1.json`. The wiring
/// slice in bd-21xbi surfaces it through
/// `data.search.lexicalRamTier.schema`.
pub const STATUS_SEARCH_LEXICAL_RAM_TIER_SCHEMA_V1: &str = "ee.status.search.lexical_ram_tier.v1";

/// Coarse host classification for the lexical RAM-tier optimization.
/// Linux is the only platform that exposes the full `mmap + mlock +
/// MADV_HUGEPAGE` triple; macOS exposes `madvise(MADV_WILLNEED)` + `mlock`
/// but no transparent-hugepage API, and Windows offers no equivalent
/// without going through `VirtualLock` / `VirtualAllocEx` which the
/// optimization deliberately does not pull in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalRamTierPlatform {
    Linux,
    MacosLimited,
    WindowsLimited,
    OtherUnsupported,
}

impl LexicalRamTierPlatform {
    #[must_use]
    pub fn detect() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacosLimited
        } else if cfg!(target_os = "windows") {
            Self::WindowsLimited
        } else {
            Self::OtherUnsupported
        }
    }

    /// True iff the platform can fully back the optimization (mmap with
    /// `MAP_POPULATE`, `mlock`, AND `MADV_HUGEPAGE`). Linux only.
    #[must_use]
    pub fn supports_full_pinning(self) -> bool {
        matches!(self, Self::Linux)
    }

    /// True iff the platform exposes at least `madvise(MADV_WILLNEED)` and
    /// `mlock` (Linux + macOS). The bd-21xbi wiring slice can offer a
    /// degraded pinning path on these hosts even without hugepages.
    #[must_use]
    pub fn supports_basic_pinning(self) -> bool {
        matches!(self, Self::Linux | Self::MacosLimited)
    }
}

/// Operator-facing configuration. Defaults are conservative: pinning is
/// `enabled=true` (per bd-21xbi the cost of an unrealized scaffold is
/// zero) but `request_hugepages` defaults to `false` because the THP
/// configuration is host-specific and the kernel can return EINVAL for
/// `MADV_HUGEPAGE` on file-backed mmaps depending on tunables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalRamTierConfig {
    pub enabled: bool,
    pub request_hugepages: bool,
    pub populate_on_open: bool,
}

impl Default for LexicalRamTierConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            request_hugepages: false,
            populate_on_open: true,
        }
    }
}

impl LexicalRamTierConfig {
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_request_hugepages(mut self, request: bool) -> Self {
        self.request_hugepages = request;
        self
    }

    #[must_use]
    pub fn with_populate_on_open(mut self, populate: bool) -> Self {
        self.populate_on_open = populate;
        self
    }
}

/// Coarse fallback strategy the loader took when the RAM-tier
/// optimization could not be fully applied. The variants are stable so
/// an operator inspecting `ee status --json` can tell at a glance why
/// the lexical index is not RAM-resident.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalRamTierFallbackPath {
    /// No fallback was taken — the index is fully RAM-pinned with the
    /// requested hugepage and populate posture.
    None,
    /// Linux scaffold path that intentionally does not call any of the
    /// `mmap`/`mlock`/`madvise` syscalls yet; the wiring slice replaces
    /// this with a real syscall path.
    SoftwareNotImplemented,
    /// macOS / other supports-basic-pinning hosts use
    /// `madvise(MADV_WILLNEED)` + optional `mlock`. The scaffold records
    /// the intended fallback path so the wiring slice can adopt it
    /// without renaming the JSON enum.
    MadviseWillneed,
    /// Windows / other unsupported platforms fall through to plain
    /// page-cache deserialization with no advice.
    HeapOnly,
    /// Operator explicitly disabled the optimization.
    DisabledByOperator,
}

/// Outcome of attempting to pin lexical index files. Shape is flat,
/// `Serialize`-derived, and camelCase so the wiring slice can drop it
/// straight into `data.search.lexicalRamTier` of `ee status --json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LexicalRamTierResult {
    pub schema: &'static str,
    pub platform: LexicalRamTierPlatform,
    pub supported: bool,
    pub enabled: bool,
    pub attempted: bool,
    pub succeeded: bool,
    pub hugepages_requested: bool,
    pub hugepages_granted: bool,
    pub populate_requested: bool,
    pub bytes_mmapped: u64,
    pub page_faults_pre: u64,
    pub page_faults_post: u64,
    pub fallback_path: LexicalRamTierFallbackPath,
    pub index_path: Option<PathBuf>,
    pub degraded_codes: Vec<String>,
}

impl LexicalRamTierResult {
    fn base(
        platform: LexicalRamTierPlatform,
        config: &LexicalRamTierConfig,
        index_path: &Path,
    ) -> Self {
        Self {
            schema: STATUS_SEARCH_LEXICAL_RAM_TIER_SCHEMA_V1,
            platform,
            supported: platform.supports_full_pinning(),
            enabled: config.enabled,
            attempted: false,
            succeeded: false,
            hugepages_requested: config.request_hugepages,
            hugepages_granted: false,
            populate_requested: config.populate_on_open,
            bytes_mmapped: 0,
            page_faults_pre: 0,
            page_faults_post: 0,
            fallback_path: LexicalRamTierFallbackPath::None,
            index_path: Some(index_path.to_path_buf()),
            degraded_codes: Vec::new(),
        }
    }

    fn push_unique_code(&mut self, code: &str) {
        if !self.degraded_codes.iter().any(|existing| existing == code) {
            self.degraded_codes.push(code.to_string());
        }
    }
}

/// Return the coarse RAM-tier support classification for the running
/// host. Cheap, allocation-free, deterministic per (target_os, build
/// configuration).
#[must_use]
pub fn platform_support() -> LexicalRamTierPlatform {
    LexicalRamTierPlatform::detect()
}

/// Attempt to pin the lexical index files under `index_dir` into the
/// page-tier of RAM indicated by `config`. The scaffold never panics,
/// never mutates the filesystem, never claims pinning succeeded that
/// did not, and never issues a real `mmap` / `mlock` / `madvise`
/// syscall — every non-success path populates `degraded_codes` with a
/// code documented in `tests/fixtures/failure_modes/`. The Linux
/// syscall implementation lives in a follow-up slice of bd-21xbi.
pub fn pin_lexical_index_files(
    index_dir: &Path,
    config: &LexicalRamTierConfig,
) -> LexicalRamTierResult {
    let platform = LexicalRamTierPlatform::detect();
    let mut result = LexicalRamTierResult::base(platform, config, index_dir);

    if !config.enabled {
        result.fallback_path = LexicalRamTierFallbackPath::DisabledByOperator;
        result.push_unique_code(LEXICAL_RAM_TIER_DISABLED_CODE);
        return result;
    }

    if config.request_hugepages && !platform.supports_full_pinning() {
        result.push_unique_code(LEXICAL_HUGEPAGES_UNAVAILABLE_CODE);
    }

    match platform {
        LexicalRamTierPlatform::Linux => {
            result.attempted = true;
            result.fallback_path = LexicalRamTierFallbackPath::SoftwareNotImplemented;
            result.push_unique_code(LEXICAL_RAM_TIER_NOT_IMPLEMENTED_CODE);
            result
        }
        LexicalRamTierPlatform::MacosLimited => {
            result.fallback_path = LexicalRamTierFallbackPath::MadviseWillneed;
            result.push_unique_code(LEXICAL_RAM_TIER_NOT_IMPLEMENTED_CODE);
            result
        }
        LexicalRamTierPlatform::WindowsLimited | LexicalRamTierPlatform::OtherUnsupported => {
            result.fallback_path = LexicalRamTierFallbackPath::HeapOnly;
            result.push_unique_code(LEXICAL_RAM_TIER_NOT_IMPLEMENTED_CODE);
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        LEXICAL_HUGEPAGES_UNAVAILABLE_CODE, LEXICAL_RAM_TIER_DISABLED_CODE,
        LEXICAL_RAM_TIER_NOT_IMPLEMENTED_CODE, LexicalRamTierConfig, LexicalRamTierFallbackPath,
        LexicalRamTierPlatform, LexicalRamTierResult, STATUS_SEARCH_LEXICAL_RAM_TIER_SCHEMA_V1,
        pin_lexical_index_files, platform_support,
    };

    fn fake_index_dir() -> &'static Path {
        Path::new("/tmp/ee-lexical-ram-tier-fake-index")
    }

    fn assert_no_duplicate_codes(result: &LexicalRamTierResult) {
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
    fn default_config_is_enabled_and_no_hugepages() {
        let config = LexicalRamTierConfig::default();
        assert!(config.enabled);
        assert!(!config.request_hugepages);
        assert!(config.populate_on_open);
    }

    #[test]
    fn disabled_config_short_circuits_with_disabled_code() {
        let result = pin_lexical_index_files(fake_index_dir(), &LexicalRamTierConfig::disabled());
        assert!(!result.enabled);
        assert!(!result.attempted);
        assert!(!result.succeeded);
        assert_eq!(
            result.fallback_path,
            LexicalRamTierFallbackPath::DisabledByOperator
        );
        assert_eq!(
            result.degraded_codes,
            vec![LEXICAL_RAM_TIER_DISABLED_CODE.to_string()]
        );
        assert_no_duplicate_codes(&result);
    }

    #[test]
    fn platform_support_is_consistent_with_cfg() {
        let platform = platform_support();
        if cfg!(target_os = "linux") {
            assert_eq!(platform, LexicalRamTierPlatform::Linux);
            assert!(platform.supports_full_pinning());
            assert!(platform.supports_basic_pinning());
        } else if cfg!(target_os = "macos") {
            assert_eq!(platform, LexicalRamTierPlatform::MacosLimited);
            assert!(!platform.supports_full_pinning());
            assert!(platform.supports_basic_pinning());
        } else if cfg!(target_os = "windows") {
            assert_eq!(platform, LexicalRamTierPlatform::WindowsLimited);
            assert!(!platform.supports_full_pinning());
            assert!(!platform.supports_basic_pinning());
        } else {
            assert_eq!(platform, LexicalRamTierPlatform::OtherUnsupported);
            assert!(!platform.supports_full_pinning());
            assert!(!platform.supports_basic_pinning());
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_platform_returns_not_implemented_code() {
        let result = pin_lexical_index_files(fake_index_dir(), &LexicalRamTierConfig::default());
        assert!(!result.supported);
        assert!(result.enabled);
        assert!(!result.attempted);
        assert!(!result.succeeded);
        assert_eq!(result.bytes_mmapped, 0);
        assert_eq!(result.page_faults_pre, 0);
        assert_eq!(result.page_faults_post, 0);
        assert!(matches!(
            result.fallback_path,
            LexicalRamTierFallbackPath::MadviseWillneed | LexicalRamTierFallbackPath::HeapOnly
        ));
        assert!(
            result
                .degraded_codes
                .iter()
                .any(|code| code == LEXICAL_RAM_TIER_NOT_IMPLEMENTED_CODE)
        );
        assert_no_duplicate_codes(&result);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_scaffold_reports_not_implemented_without_claiming_success() {
        let result = pin_lexical_index_files(fake_index_dir(), &LexicalRamTierConfig::default());
        assert_eq!(result.platform, LexicalRamTierPlatform::Linux);
        assert!(result.supported);
        assert!(result.enabled);
        assert!(result.attempted);
        assert!(!result.succeeded, "scaffold must not claim success");
        assert!(
            !result.hugepages_granted,
            "scaffold must not claim THP granted"
        );
        assert_eq!(result.bytes_mmapped, 0);
        assert_eq!(
            result.fallback_path,
            LexicalRamTierFallbackPath::SoftwareNotImplemented
        );
        assert!(
            result
                .degraded_codes
                .iter()
                .any(|code| code == LEXICAL_RAM_TIER_NOT_IMPLEMENTED_CODE)
        );
        assert_no_duplicate_codes(&result);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn requesting_hugepages_on_unsupported_platform_emits_unavailable_code() {
        let config = LexicalRamTierConfig::default().with_request_hugepages(true);
        let result = pin_lexical_index_files(fake_index_dir(), &config);
        assert!(result.hugepages_requested);
        assert!(!result.hugepages_granted);
        assert!(
            result
                .degraded_codes
                .iter()
                .any(|code| code == LEXICAL_HUGEPAGES_UNAVAILABLE_CODE)
        );
        assert_no_duplicate_codes(&result);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn requesting_hugepages_on_linux_does_not_emit_unavailable_code() {
        let config = LexicalRamTierConfig::default().with_request_hugepages(true);
        let result = pin_lexical_index_files(fake_index_dir(), &config);
        assert!(result.hugepages_requested);
        assert!(
            !result
                .degraded_codes
                .iter()
                .any(|code| code == LEXICAL_HUGEPAGES_UNAVAILABLE_CODE),
            "linux should not emit hugepages_unavailable; got {:?}",
            result.degraded_codes
        );
    }

    #[test]
    fn result_schema_matches_documented_id() {
        let result = pin_lexical_index_files(fake_index_dir(), &LexicalRamTierConfig::default());
        assert_eq!(result.schema, STATUS_SEARCH_LEXICAL_RAM_TIER_SCHEMA_V1);
        assert_eq!(
            STATUS_SEARCH_LEXICAL_RAM_TIER_SCHEMA_V1,
            "ee.status.search.lexical_ram_tier.v1"
        );
    }

    #[test]
    fn config_builder_methods_round_trip() {
        let config = LexicalRamTierConfig::default()
            .with_request_hugepages(true)
            .with_populate_on_open(false);
        assert!(config.request_hugepages);
        assert!(!config.populate_on_open);
        assert!(config.enabled);
    }

    #[test]
    fn pin_lexical_index_files_preserves_index_path_in_result() {
        let path = Path::new("/var/lib/ee/indexes/combined/lexical");
        let result = pin_lexical_index_files(path, &LexicalRamTierConfig::default());
        assert_eq!(result.index_path.as_deref(), Some(path));
    }

    #[test]
    fn result_serializes_with_camel_case_fields() {
        let result = pin_lexical_index_files(fake_index_dir(), &LexicalRamTierConfig::disabled());
        let serialized = serde_json::to_value(&result).expect("serialize result");
        for key in [
            "schema",
            "platform",
            "supported",
            "enabled",
            "attempted",
            "succeeded",
            "hugepagesRequested",
            "hugepagesGranted",
            "populateRequested",
            "bytesMmapped",
            "pageFaultsPre",
            "pageFaultsPost",
            "fallbackPath",
            "indexPath",
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
