//! Lexical posting-list RAM-tier warmload (bd-1hvzh / bd-21xbi).
//!
//! On 256GB+ Linux hosts the Frankensearch lexical index files under
//! `indexes/combined/` can pay disk page-fault cost on first touch. Under
//! this crate's `forbid(unsafe_code)` policy the supported V1 posture is a
//! process-local heap warmload: retain the lexical index bytes in memory,
//! report `lexical_ram_tier_heap_warmload`, and never claim OS-level
//! `mmap`/`mlock` pinning happened. This is distinct from bd-1prrl.3 /
//! swarmx.4 (graph snapshot mmap), bd-ndzfg (L2 pack result cache), and
//! bd-168gm (embedding LRU) — each warms or pins a different dataset.
//!
//! This module owns the platform-agnostic public surface: configuration
//! types, the result envelope surfaced under `ee status --json` →
//! `data.search.lexicalRamTier`, the degraded-code vocabulary, and the
//! entry point the index loader calls. The historical
//! `lexical_ram_tier_not_implemented` honesty code is retired; live Linux
//! behavior is heap warmload with `succeeded=false`, `bytesMmapped=0`, and
//! `bytesWarmloaded>0`.
//!
//! Determinism contract: the optimization only changes wall-clock and
//! page-cache residency; lexical search results MUST be byte-identical
//! whether the index is heap-warmloaded or read from disk. The wiring slice
//! extends `tests/determinism_unit.rs` with the `pin_ram` × `request_hugepages`
//! dimensions.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::UNIX_EPOCH,
};

use serde::Serialize;

use crate::models::CorpusRevision;

/// `degraded[]` code emitted when an operator requested transparent
/// hugepages but the current platform arm cannot even attempt
/// `MADV_HUGEPAGE` (every non-Linux host, plus any live heap-warmload path
/// that cannot make a syscall-level hugepage request). The V1 contract does
/// not probe Linux THP state.
pub const LEXICAL_HUGEPAGES_UNAVAILABLE_CODE: &str = "lexical_hugepages_unavailable";

/// `degraded[]` code emitted when an operator has disabled the
/// optimization through `[search.lexical_ram_tier] enabled = false` (or
/// the env-var equivalent). Response-time classification per
/// `docs/degraded_code_taxonomy.md`.
pub const LEXICAL_RAM_TIER_DISABLED_CODE: &str = "lexical_ram_tier_disabled";

/// `degraded[]` code emitted when the safe adapter retains lexical index
/// bytes in process heap memory instead of claiming OS-level
/// `mmap`/`mlock` pinning. This is a real opt-in warmload path, but
/// consumers MUST treat it as degraded relative to full RAM pinning:
/// search results are unchanged, hugepages are not granted, and the
/// process may still be paged out by the kernel.
pub const LEXICAL_RAM_TIER_HEAP_WARMLOAD_CODE: &str = "lexical_ram_tier_heap_warmload";

/// `degraded[]` code emitted on macOS hosts where the lexical RAM-tier
/// optimization is intentionally a no-op. macOS exposes
/// `madvise(MADV_WILLNEED)` + `mlock` but no transparent-hugepage API,
/// so a Linux-equivalent pinning shape cannot be reproduced; the
/// optimization remains experimental on the Mac dev path while the
/// production target stays Linux 256GB+ hosts (bd-21xbi parent).
///
/// Distinguishing this from the Linux
/// [`LEXICAL_RAM_TIER_HEAP_WARMLOAD_CODE`] lets agents and operators
/// see at a glance that the Mac dev host is not the target host class
/// rather than "Linux fell back to process-local warmload" — separate
/// remediations for separate root causes. Pairs with the per-platform
/// taxonomy row in `docs/degraded_code_taxonomy.md` and the fixture at
/// `tests/fixtures/failure_modes/lexical_ram_unavailable_on_macos.json`.
pub const LEXICAL_RAM_UNAVAILABLE_ON_MACOS_CODE: &str = "lexical_ram_unavailable_on_macos";

/// Forward-looking schema id for the `ee status --json` lexicalRamTier
/// block, kept in sync with
/// `docs/schemas/ee.status.search.lexical_ram_tier.v1.json`. The wiring
/// slice in bd-21xbi surfaces it through
/// `data.search.lexicalRamTier.schema`.
pub const STATUS_SEARCH_LEXICAL_RAM_TIER_SCHEMA_V1: &str = "ee.status.search.lexical_ram_tier.v1";

/// Environment-variable name registered in `src/config/env_registry.rs`.
/// Exposed here as a public constant so
/// [`LexicalRamTierConfig::from_environment_with_reader`] keeps the
/// canonical spelling in one place: the registry row reads
/// `EnvVar::LexicalIndexPinRam.name()`, the docs/env_vars.md row will
/// match this constant, and tests can avoid hard-coding string literals.
pub const LEXICAL_RAM_TIER_PIN_RAM_ENV: &str = "EE_LEXICAL_INDEX_PIN_RAM";

/// Companion env-var name for the transparent-hugepages opt-in.
/// Only effective when `EE_LEXICAL_INDEX_PIN_RAM=1` and the platform arm can
/// attempt transparent-hugepage advice; otherwise the loader emits
/// [`LEXICAL_HUGEPAGES_UNAVAILABLE_CODE`]. The current heap-warmload contract
/// does not probe Linux THP state.
pub const LEXICAL_RAM_TIER_HUGEPAGES_ENV: &str = "EE_LEXICAL_INDEX_HUGEPAGES";

/// Coarse host classification for the lexical RAM-tier optimization.
/// Linux is the only platform that exposes the full `mmap + mlock +
/// MADV_HUGEPAGE` triple; macOS exposes `madvise(MADV_WILLNEED)` + `mlock`
/// but no transparent-hugepage API, and Windows offers no equivalent
/// without going through `VirtualLock` / `VirtualAllocEx` which the
/// optimization deliberately does not pull in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalRamTierPlatform {
    NotCollected,
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

    /// True iff the platform exposes primitives that could fully back an
    /// OS-pinning adapter (`MAP_POPULATE`, `mlock`, and `MADV_HUGEPAGE`).
    /// The live V1 implementation still reports heap warmload rather than
    /// claiming those syscalls were attempted.
    #[must_use]
    pub fn supports_full_pinning(self) -> bool {
        matches!(self, Self::Linux)
    }

    /// True iff the platform exposes at least `madvise(MADV_WILLNEED)` and
    /// `mlock` (Linux + macOS). The live V1 implementation keeps this as a
    /// host-class signal while still using the safe heap warmload path.
    #[must_use]
    pub fn supports_basic_pinning(self) -> bool {
        matches!(self, Self::Linux | Self::MacosLimited)
    }
}

/// Operator-facing configuration. Defaults are conservative: RAM-tier warmload is
/// opt-in (`enabled=false`) and `request_hugepages` defaults to `false`
/// because the THP configuration is host-specific and the kernel can
/// return EINVAL for `MADV_HUGEPAGE` on file-backed mmaps depending on
/// tunables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalRamTierConfig {
    pub enabled: bool,
    pub request_hugepages: bool,
    pub populate_on_open: bool,
}

impl Default for LexicalRamTierConfig {
    fn default() -> Self {
        Self {
            enabled: false,
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

    /// Build the runtime posture from the typed config surface.
    ///
    /// The config surface stores optional fields so partially-specified
    /// config files and layered merges can share one type. Missing fields keep
    /// the runtime defaults. Requesting hugepages while the parent RAM tier is
    /// disabled is normalized back to `false`, matching the environment
    /// reader's semantic guard.
    #[must_use]
    pub fn from_config_overrides(overrides: &crate::config::SearchLexicalRamTierConfig) -> Self {
        let mut config = Self::default();
        if let Some(enabled) = overrides.enabled {
            config.enabled = enabled;
        }
        if let Some(request_hugepages) = overrides.request_hugepages {
            config.request_hugepages = request_hugepages;
        }
        if let Some(populate_on_open) = overrides.populate_on_open {
            config.populate_on_open = populate_on_open;
        }
        config.force_hugepages_off_when_disabled();
        config
    }

    /// Build a config from an arbitrary env-var reader.
    ///
    /// The reader takes the canonical env-var name (matching
    /// [`LEXICAL_RAM_TIER_PIN_RAM_ENV`] / [`LEXICAL_RAM_TIER_HUGEPAGES_ENV`])
    /// and returns the configured value. Production callers pass the config
    /// registry reader for the corresponding `EnvVar` variants; the reader
    /// indirection lets tests inject deterministic values without touching the
    /// process environment.
    ///
    /// Boolean parsing accepts the same lenient vocabulary `parse_bool_arg`
    /// uses for CLI flags: `true` / `1` / `yes` / `on` → true and
    /// `false` / `0` / `no` / `off` → false. Casing is folded. Any other
    /// non-empty value invokes the `on_unparseable` callback with the var
    /// name and raw value so the caller can record a degraded code; the
    /// config field then retains its default. Missing values keep the
    /// existing default (`enabled = false`, `request_hugepages = false`,
    /// `populate_on_open = true`).
    ///
    /// Semantic precondition: requesting hugepages only makes sense when
    /// pinning is enabled. If `EE_LEXICAL_INDEX_HUGEPAGES=true` is observed
    /// while `EE_LEXICAL_INDEX_PIN_RAM=false` is also observed (i.e. the
    /// operator explicitly disabled the parent), `request_hugepages` is
    /// forced back to `false` and `on_unparseable` is invoked with a
    /// "requires-pin-ram" rationale so the caller can surface the
    /// inconsistency through the existing `lexical_hugepages_unavailable`
    /// path. This keeps `pin_lexical_index_files` honest: it never claims
    /// hugepages were requested on a disabled tier.
    #[must_use]
    pub fn from_environment_with_reader<F, G>(reader: F, mut on_unparseable: G) -> Self
    where
        F: Fn(&'static str) -> Option<String>,
        G: FnMut(&'static str, &str),
    {
        let mut config = Self::default();

        let pin_ram_raw = reader(LEXICAL_RAM_TIER_PIN_RAM_ENV);
        let pin_ram_value = pin_ram_raw
            .as_deref()
            .and_then(|raw| match parse_env_bool(raw) {
                Some(parsed) => Some(parsed),
                None => {
                    on_unparseable(LEXICAL_RAM_TIER_PIN_RAM_ENV, raw);
                    None
                }
            });
        if let Some(value) = pin_ram_value {
            config.enabled = value;
        }

        let hugepages_raw = reader(LEXICAL_RAM_TIER_HUGEPAGES_ENV);
        let hugepages_value = hugepages_raw
            .as_deref()
            .and_then(|raw| match parse_env_bool(raw) {
                Some(parsed) => Some(parsed),
                None => {
                    on_unparseable(LEXICAL_RAM_TIER_HUGEPAGES_ENV, raw);
                    None
                }
            });
        if let Some(value) = hugepages_value {
            config.request_hugepages = value;
        }

        // Hugepages without pinning is a no-op at the syscall level (you
        // can't MADV_HUGEPAGE a region that was never mmap'd by the
        // optimization). Force the field back to false and tell the caller
        // why so they can record the inconsistency through degraded[].
        if config.force_hugepages_off_when_disabled() {
            on_unparseable(LEXICAL_RAM_TIER_HUGEPAGES_ENV, "requires-pin-ram-enabled");
        }

        config
    }

    fn force_hugepages_off_when_disabled(&mut self) -> bool {
        if self.request_hugepages && !self.enabled {
            self.request_hugepages = false;
            true
        } else {
            false
        }
    }
}

/// Emit the required lexical RAM-tier trace fields in a single place so
/// status and search wiring stay aligned.
pub fn trace_lexical_ram_tier(
    workspace_id: &str,
    result: &LexicalRamTierResult,
    elapsed_ms_first_search: f64,
) {
    let index_path = result
        .index_path
        .as_deref()
        .map(Path::display)
        .map(|display| display.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let index_revision = result
        .index_revision
        .as_ref()
        .map(CorpusRevision::as_str)
        .unwrap_or("unknown");
    // The target is pinned explicitly rather than inherited from
    // `module_path!()`: `core::search` guards this call with
    // `tracing::enabled!(target: "ee::search::lexical_ram_tier", ...)` to avoid
    // resolving the bound workspace id (a fresh database open) when nobody is
    // listening. If the target were implicit, moving this function would
    // silently stop the event from ever being emitted.
    tracing::info!(
        target: "ee::search::lexical_ram_tier",
        surface = "lexical_ram_tier",
        workspace_id,
        index_path = %index_path,
        index_revision,
        bytes_mmapped = result.bytes_mmapped,
        bytes_warmloaded = result.bytes_warmloaded,
        hugepages_requested = result.hugepages_requested,
        hugepages_granted = result.hugepages_granted,
        page_faults_pre = result.page_faults_pre,
        page_faults_post = result.page_faults_post,
        elapsed_ms_first_search,
        degraded_codes = ?result.degraded_codes,
        "lexical RAM-tier posture"
    );
}

/// Lenient boolean parser shared with the CLI flag parser. Accepts the
/// canonical operator vocabulary (`true`/`false`, `1`/`0`, `yes`/`no`,
/// `on`/`off`) with case folding. Returns `None` for any other token so
/// the caller can decide whether to emit a degraded code or fall back to
/// the default.
fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Coarse fallback strategy the loader took when the RAM-tier
/// optimization could not be fully applied. The variants are stable so
/// an operator inspecting `ee status --json` can tell at a glance why
/// the lexical index is not RAM-resident.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalRamTierFallbackPath {
    /// No fallback was taken. Reserved for a future explicitly-scoped
    /// OS-pinning adapter; the V1 heap-warmload path does not produce it.
    None,
    /// Safe process-local heap mirror used when the crate-level
    /// `forbid(unsafe_code)` policy prevents direct `mmap`/`mlock`
    /// syscalls in this package. This is a real warmload, not a full
    /// OS pin.
    HeapWarmload,
    /// macOS / other supports-basic-pinning hosts use
    /// `madvise(MADV_WILLNEED)` + optional `mlock`. The historical enum
    /// name is preserved so existing status consumers do not need a schema
    /// migration; the macOS arm does not attempt a syscall adapter.
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
    pub collection_status: &'static str,
    pub platform: LexicalRamTierPlatform,
    pub supported: bool,
    pub enabled: bool,
    pub attempted: bool,
    pub succeeded: bool,
    pub hugepages_requested: bool,
    pub hugepages_granted: bool,
    pub populate_requested: bool,
    pub bytes_mmapped: u64,
    pub bytes_warmloaded: u64,
    pub page_faults_pre: u64,
    pub page_faults_post: u64,
    pub fallback_path: LexicalRamTierFallbackPath,
    pub index_path: Option<PathBuf>,
    pub index_revision: Option<CorpusRevision>,
    pub degraded_codes: Vec<String>,
    // O(1) duplicate checks while `degraded_codes` preserves JSON order.
    #[serde(skip)]
    degraded_code_set: HashSet<&'static str>,
}

impl LexicalRamTierResult {
    fn base(
        platform: LexicalRamTierPlatform,
        config: &LexicalRamTierConfig,
        index_path: &Path,
    ) -> Self {
        Self {
            schema: STATUS_SEARCH_LEXICAL_RAM_TIER_SCHEMA_V1,
            collection_status: "observed",
            platform,
            supported: platform.supports_full_pinning(),
            enabled: config.enabled,
            attempted: false,
            succeeded: false,
            hugepages_requested: config.request_hugepages,
            hugepages_granted: false,
            populate_requested: config.populate_on_open,
            bytes_mmapped: 0,
            bytes_warmloaded: 0,
            page_faults_pre: 0,
            page_faults_post: 0,
            fallback_path: LexicalRamTierFallbackPath::None,
            index_path: Some(index_path.to_path_buf()),
            index_revision: None,
            degraded_codes: Vec::new(),
            degraded_code_set: HashSet::new(),
        }
    }

    /// Return an explicit, allocation-light posture for status surfaces that
    /// intentionally skip optional index I/O.
    #[must_use]
    pub fn not_collected() -> Self {
        let platform = LexicalRamTierPlatform::NotCollected;
        Self {
            schema: STATUS_SEARCH_LEXICAL_RAM_TIER_SCHEMA_V1,
            collection_status: "not_collected",
            platform,
            supported: platform.supports_full_pinning(),
            enabled: false,
            attempted: false,
            succeeded: false,
            hugepages_requested: false,
            hugepages_granted: false,
            populate_requested: false,
            bytes_mmapped: 0,
            bytes_warmloaded: 0,
            page_faults_pre: 0,
            page_faults_post: 0,
            fallback_path: LexicalRamTierFallbackPath::None,
            index_path: None,
            index_revision: None,
            degraded_codes: Vec::new(),
            degraded_code_set: HashSet::new(),
        }
    }

    fn push_unique_code(&mut self, code: &'static str) {
        if self.degraded_code_set.insert(code) {
            self.degraded_codes.push(code.to_owned());
        }
    }
}

#[derive(Default)]
struct LexicalRamTierHeapCache {
    revision: Option<CorpusRevision>,
    buffers: Vec<Vec<u8>>,
    bytes: u64,
}

static LEXICAL_RAM_TIER_HEAP_CACHE: OnceLock<Mutex<LexicalRamTierHeapCache>> = OnceLock::new();

#[derive(Debug)]
struct LexicalIndexRevisionEntry {
    relative_path: String,
    kind: &'static str,
    len: u64,
    modified: Option<(u64, u32)>,
}

fn lexical_index_revision(index_dir: &Path) -> Option<CorpusRevision> {
    let mut entries = Vec::new();
    collect_lexical_index_revision_entries(index_dir, index_dir, &mut entries).ok()?;
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.lexical_ram_tier.index_revision.v1\0");
    hasher.update(&(entries.len() as u64).to_le_bytes());
    for entry in entries {
        hasher.update(entry.relative_path.as_bytes());
        hasher.update(&[0]);
        hasher.update(entry.kind.as_bytes());
        hasher.update(&[0]);
        hasher.update(&entry.len.to_le_bytes());
        match entry.modified {
            Some((secs, nanos)) => {
                hasher.update(&secs.to_le_bytes());
                hasher.update(&nanos.to_le_bytes());
            }
            None => {
                hasher.update(b"modified_unknown");
            }
        }
        hasher.update(&[0xff]);
    }

    Some(CorpusRevision::new(format!(
        "lexical:{}",
        hasher.finalize().to_hex()
    )))
}

fn collect_lexical_index_revision_entries(
    root: &Path,
    path: &Path,
    entries: &mut Vec<LexicalIndexRevisionEntry>,
) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    entries.push(lexical_index_revision_entry(root, path, &metadata));
    if metadata.is_dir() {
        let mut children = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(std::fs::DirEntry::path);
        for child in children {
            collect_lexical_index_revision_entries(root, &child.path(), entries)?;
        }
    }
    Ok(())
}

fn lexical_index_revision_entry(
    root: &Path,
    path: &Path,
    metadata: &fs::Metadata,
) -> LexicalIndexRevisionEntry {
    let relative_path = match path.strip_prefix(root) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
        Ok(relative) => relative.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    };
    let file_type = metadata.file_type();
    let kind = if file_type.is_dir() {
        "dir"
    } else if file_type.is_file() {
        "file"
    } else if file_type.is_symlink() {
        "symlink"
    } else {
        "other"
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| (duration.as_secs(), duration.subsec_nanos()));
    LexicalIndexRevisionEntry {
        relative_path,
        kind,
        len: metadata.len(),
        modified,
    }
}

/// Return the coarse RAM-tier support classification for the running
/// host. Cheap, allocation-free, deterministic per (target_os, build
/// configuration).
#[must_use]
pub fn platform_support() -> LexicalRamTierPlatform {
    LexicalRamTierPlatform::detect()
}

/// Attempt the lexical RAM-tier optimization for the index files under
/// `index_dir`. The live V1 contract is a safe heap warmload, not
/// OS-level pinning: this function never panics, never mutates the
/// filesystem, never claims pinning succeeded when it did not, and never
/// issues a real `mmap` / `mlock` / `madvise` syscall. Every non-success
/// path populates `degraded_codes` with a code documented in
/// `tests/fixtures/failure_modes/`.
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

    result.index_revision = lexical_index_revision(index_dir);

    if config.request_hugepages && !platform.supports_full_pinning() {
        result.push_unique_code(LEXICAL_HUGEPAGES_UNAVAILABLE_CODE);
    }

    match platform {
        LexicalRamTierPlatform::Linux => {
            result.attempted = true;
            result.fallback_path = LexicalRamTierFallbackPath::HeapWarmload;
            if let Ok(bytes) =
                warmload_lexical_index_files(index_dir, result.index_revision.as_ref())
            {
                result.bytes_warmloaded = bytes;
            }
            result.push_unique_code(LEXICAL_RAM_TIER_HEAP_WARMLOAD_CODE);
            result
        }
        LexicalRamTierPlatform::MacosLimited => {
            // macOS no-op per bd-21xbi.2: surface the platform-specific
            // `lexical_ram_unavailable_on_macos` degraded code so
            // operators can distinguish "wrong host class for this
            // optimization" from the Linux heap-warmload path. The
            // MadviseWillneed fallback name is preserved because
            // The historical fallback name remains in the public enum;
            // this arm makes the not-attempted reality explicit.
            result.fallback_path = LexicalRamTierFallbackPath::MadviseWillneed;
            result.push_unique_code(LEXICAL_RAM_UNAVAILABLE_ON_MACOS_CODE);
            result
        }
        LexicalRamTierPlatform::NotCollected => result,
        LexicalRamTierPlatform::WindowsLimited | LexicalRamTierPlatform::OtherUnsupported => {
            result.attempted = true;
            result.fallback_path = LexicalRamTierFallbackPath::HeapOnly;
            if let Ok(bytes) =
                warmload_lexical_index_files(index_dir, result.index_revision.as_ref())
            {
                result.bytes_warmloaded = bytes;
            }
            result.push_unique_code(LEXICAL_RAM_TIER_HEAP_WARMLOAD_CODE);
            result
        }
    }
}

fn warmload_lexical_index_files(
    index_dir: &Path,
    revision: Option<&CorpusRevision>,
) -> std::io::Result<u64> {
    let mut buffers = Vec::new();
    collect_lexical_index_file_bytes(index_dir, &mut buffers)?;
    let bytes = buffers.iter().map(|buffer| buffer.len() as u64).sum();
    let cache =
        LEXICAL_RAM_TIER_HEAP_CACHE.get_or_init(|| Mutex::new(LexicalRamTierHeapCache::default()));
    let mut guard = match cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.revision = revision.cloned();
    guard.bytes = bytes;
    guard.buffers = buffers;
    Ok(bytes)
}

fn collect_lexical_index_file_bytes(
    path: &Path,
    buffers: &mut Vec<Vec<u8>>,
) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() {
        buffers.push(fs::read(path)?);
        return Ok(());
    }
    if metadata.is_dir() {
        let mut children = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(std::fs::DirEntry::path);
        for child in children {
            collect_lexical_index_file_bytes(&child.path(), buffers)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::{
        LEXICAL_HUGEPAGES_UNAVAILABLE_CODE, LEXICAL_RAM_TIER_DISABLED_CODE,
        LEXICAL_RAM_TIER_HEAP_WARMLOAD_CODE, LEXICAL_RAM_UNAVAILABLE_ON_MACOS_CODE,
        LexicalRamTierConfig, LexicalRamTierFallbackPath, LexicalRamTierPlatform,
        LexicalRamTierResult, STATUS_SEARCH_LEXICAL_RAM_TIER_SCHEMA_V1, pin_lexical_index_files,
        platform_support,
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
    fn default_config_is_disabled_and_no_hugepages() {
        let config = LexicalRamTierConfig::default();
        assert!(!config.enabled);
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
            vec![LEXICAL_RAM_TIER_DISABLED_CODE.to_owned()]
        );
        assert_no_duplicate_codes(&result);
    }

    #[test]
    fn degraded_code_sidecar_preserves_order_and_filters_duplicates() {
        let mut result = LexicalRamTierResult::base(
            LexicalRamTierPlatform::MacosLimited,
            &LexicalRamTierConfig::default(),
            fake_index_dir(),
        );

        result.push_unique_code(LEXICAL_HUGEPAGES_UNAVAILABLE_CODE);
        result.push_unique_code(LEXICAL_RAM_UNAVAILABLE_ON_MACOS_CODE);
        result.push_unique_code(LEXICAL_HUGEPAGES_UNAVAILABLE_CODE);

        assert_eq!(
            result.degraded_codes,
            vec![
                LEXICAL_HUGEPAGES_UNAVAILABLE_CODE.to_owned(),
                LEXICAL_RAM_UNAVAILABLE_ON_MACOS_CODE.to_owned(),
            ]
        );
        assert_eq!(result.degraded_code_set.len(), 2);
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
    fn non_linux_platform_returns_platform_specific_degraded_code() {
        let result = pin_lexical_index_files(
            fake_index_dir(),
            &LexicalRamTierConfig {
                enabled: true,
                ..LexicalRamTierConfig::default()
            },
        );
        assert!(!result.supported);
        assert!(result.enabled);
        if cfg!(target_os = "macos") {
            assert!(!result.attempted);
        } else {
            assert!(result.attempted);
        }
        assert!(!result.succeeded);
        assert_eq!(result.bytes_mmapped, 0);
        assert_eq!(result.page_faults_pre, 0);
        assert_eq!(result.page_faults_post, 0);
        assert!(matches!(
            result.fallback_path,
            LexicalRamTierFallbackPath::MadviseWillneed | LexicalRamTierFallbackPath::HeapOnly
        ));
        // macOS gets the platform-specific `lexical_ram_unavailable_on_macos`
        // (bd-21xbi.2); Windows / other-unsupported keep an honest heap-only
        // degraded code until their own Mac-style platform-specific codes
        // land.
        let expected_code = if cfg!(target_os = "macos") {
            LEXICAL_RAM_UNAVAILABLE_ON_MACOS_CODE
        } else {
            LEXICAL_RAM_TIER_HEAP_WARMLOAD_CODE
        };
        assert!(
            result
                .degraded_codes
                .iter()
                .any(|code| code == expected_code),
            "expected degraded code `{expected_code}` for target_os; got {:?}",
            result.degraded_codes
        );
        assert_no_duplicate_codes(&result);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_heap_warmload_retains_bytes_without_claiming_os_pinning() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("lexical");
        fs::create_dir(&root).expect("create root");
        fs::write(root.join("postings.bin"), b"posting-list").expect("write postings");
        fs::write(root.join("terms.bin"), b"terms").expect("write terms");

        let result = pin_lexical_index_files(
            &root,
            &LexicalRamTierConfig {
                enabled: true,
                ..LexicalRamTierConfig::default()
            },
        );
        assert_eq!(result.platform, LexicalRamTierPlatform::Linux);
        assert!(result.supported);
        assert!(result.enabled);
        assert!(result.attempted);
        assert!(!result.succeeded, "scaffold must not claim success");
        assert!(
            !result.hugepages_granted,
            "heap warmload must not claim THP granted"
        );
        assert_eq!(result.bytes_mmapped, 0);
        assert_eq!(result.bytes_warmloaded, 17);
        assert_eq!(
            result.fallback_path,
            LexicalRamTierFallbackPath::HeapWarmload
        );
        assert!(
            result
                .degraded_codes
                .iter()
                .any(|code| code == LEXICAL_RAM_TIER_HEAP_WARMLOAD_CODE)
        );
        assert_no_duplicate_codes(&result);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_emits_platform_specific_lexical_ram_unavailable_code() {
        // bd-21xbi.2 platform-specific code routing. Per the bead, the
        // Mac dev path is a no-op (transparent-hugepage API absent;
        // production target is Linux 256GB+). The Mac arm emits
        // `lexical_ram_unavailable_on_macos` so operators can
        // distinguish the wrong-host-class case from the Linux
        // adapter-not-landed case without parsing a generic code.
        let result = pin_lexical_index_files(
            fake_index_dir(),
            &LexicalRamTierConfig {
                enabled: true,
                ..LexicalRamTierConfig::default()
            },
        );
        assert_eq!(result.platform, LexicalRamTierPlatform::MacosLimited);
        assert_eq!(
            result.fallback_path,
            LexicalRamTierFallbackPath::MadviseWillneed
        );
        assert!(
            result
                .degraded_codes
                .iter()
                .any(|code| code == LEXICAL_RAM_UNAVAILABLE_ON_MACOS_CODE),
            "macos must emit `lexical_ram_unavailable_on_macos`; got {:?}",
            result.degraded_codes
        );
        // The heap-warmload code MUST NOT be emitted on Mac:
        // emitting both would defeat the purpose of the platform-
        // specific routing.
        assert!(
            !result
                .degraded_codes
                .iter()
                .any(|code| code == LEXICAL_RAM_TIER_HEAP_WARMLOAD_CODE),
            "macos must NOT emit the heap warmload code; got {:?}",
            result.degraded_codes
        );
        assert_no_duplicate_codes(&result);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn requesting_hugepages_on_unsupported_platform_emits_unavailable_code() {
        let config = LexicalRamTierConfig {
            enabled: true,
            ..LexicalRamTierConfig::default()
        }
        .with_request_hugepages(true);
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
        let config = LexicalRamTierConfig {
            enabled: true,
            ..LexicalRamTierConfig::default()
        }
        .with_request_hugepages(true);
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
        let result = pin_lexical_index_files(fake_index_dir(), &LexicalRamTierConfig::disabled());
        assert_eq!(result.schema, STATUS_SEARCH_LEXICAL_RAM_TIER_SCHEMA_V1);
        assert_eq!(result.collection_status, "observed");
        assert_eq!(
            STATUS_SEARCH_LEXICAL_RAM_TIER_SCHEMA_V1,
            "ee.status.search.lexical_ram_tier.v1"
        );
    }

    #[test]
    fn not_collected_result_performs_no_index_claims() {
        let result = LexicalRamTierResult::not_collected();

        assert_eq!(result.collection_status, "not_collected");
        assert_eq!(result.platform, LexicalRamTierPlatform::NotCollected);
        assert!(!result.attempted);
        assert!(!result.succeeded);
        assert_eq!(result.fallback_path, LexicalRamTierFallbackPath::None);
        assert!(result.index_path.is_none());
        assert!(result.index_revision.is_none());
        assert!(result.degraded_codes.is_empty());
    }

    #[test]
    fn config_builder_methods_round_trip() {
        let config = LexicalRamTierConfig::default()
            .with_request_hugepages(true)
            .with_populate_on_open(false);
        assert!(config.request_hugepages);
        assert!(!config.populate_on_open);
        assert!(!config.enabled);
    }

    #[test]
    fn from_config_overrides_preserves_runtime_defaults_for_absent_fields() {
        let config = LexicalRamTierConfig::from_config_overrides(
            &crate::config::SearchLexicalRamTierConfig::default(),
        );
        assert_eq!(config, LexicalRamTierConfig::default());
    }

    #[test]
    fn from_config_overrides_applies_typed_config_values() {
        let config = LexicalRamTierConfig::from_config_overrides(
            &crate::config::SearchLexicalRamTierConfig {
                enabled: Some(true),
                request_hugepages: Some(true),
                populate_on_open: Some(false),
            },
        );
        assert!(config.enabled);
        assert!(config.request_hugepages);
        assert!(!config.populate_on_open);
    }

    #[test]
    fn from_config_overrides_forces_hugepages_off_when_disabled() {
        let config = LexicalRamTierConfig::from_config_overrides(
            &crate::config::SearchLexicalRamTierConfig {
                enabled: Some(false),
                request_hugepages: Some(true),
                populate_on_open: None,
            },
        );
        assert!(!config.enabled);
        assert!(!config.request_hugepages);
        assert!(config.populate_on_open);
    }

    #[test]
    fn pin_lexical_index_files_preserves_index_path_in_result() {
        let path = Path::new("/var/lib/ee/indexes/combined/lexical");
        let result = pin_lexical_index_files(path, &LexicalRamTierConfig::disabled());
        assert_eq!(result.index_path.as_deref(), Some(path));
    }

    #[test]
    fn disabled_result_leaves_index_revision_empty_bd_1eh60() {
        let result = pin_lexical_index_files(fake_index_dir(), &LexicalRamTierConfig::disabled());
        assert_eq!(result.index_revision, None);
    }

    #[test]
    fn enabled_result_records_stable_index_revision_bd_1eh60() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/search");
        let config = LexicalRamTierConfig {
            enabled: true,
            ..LexicalRamTierConfig::default()
        };
        let first = pin_lexical_index_files(&path, &config);
        let second = pin_lexical_index_files(&path, &config);

        let revision = first
            .index_revision
            .as_ref()
            .expect("existing index path should produce a revision");
        assert!(
            revision.as_str().starts_with("lexical:"),
            "revision must be namespaced and opaque: {revision}"
        );
        assert_eq!(first.index_revision, second.index_revision);
    }

    #[test]
    fn index_revision_entry_uses_path_independent_root_sentinel_bd_1eh60() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("index");
        fs::create_dir(&root).expect("create root index dir");
        let child = root.join("postings.bin");
        fs::write(&child, b"posting-list").expect("write child index file");

        let root_metadata = fs::symlink_metadata(&root).expect("root metadata");
        let child_metadata = fs::symlink_metadata(&child).expect("child metadata");

        let root_entry = super::lexical_index_revision_entry(&root, &root, &root_metadata);
        let child_entry = super::lexical_index_revision_entry(&root, &child, &child_metadata);

        assert_eq!(
            root_entry.relative_path, ".",
            "root entry must not encode the absolute index directory path"
        );
        assert_eq!(child_entry.relative_path, "postings.bin");
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
            "bytesWarmloaded",
            "pageFaultsPre",
            "pageFaultsPost",
            "fallbackPath",
            "indexPath",
            "indexRevision",
            "degradedCodes",
        ] {
            assert!(
                serialized.get(key).is_some(),
                "expected field {key} in serialized result {serialized}"
            );
        }
        assert!(
            serialized.get("degradedCodeSet").is_none(),
            "private degraded-code sidecar must not leak into JSON: {serialized}"
        );
        assert!(
            serialized.get("index_revision").is_none(),
            "index revision must use camelCase in JSON: {serialized}"
        );
        assert_eq!(
            serialized
                .get("fallbackPath")
                .and_then(|value| value.as_str()),
            Some("disabled_by_operator")
        );
    }

    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::{LEXICAL_RAM_TIER_HUGEPAGES_ENV, LEXICAL_RAM_TIER_PIN_RAM_ENV, parse_env_bool};

    /// Helper: turn a literal `[(name, value)]` slice into the reader
    /// closure signature [`LexicalRamTierConfig::from_environment_with_reader`]
    /// expects so the tests stay readable.
    fn env_reader_from<'a>(
        entries: &'a [(&'static str, &'static str)],
    ) -> impl Fn(&'static str) -> Option<String> + 'a {
        let map: HashMap<&'static str, &'static str> = entries.iter().copied().collect();
        move |name: &'static str| map.get(name).map(|v| (*v).to_owned())
    }

    #[test]
    fn parse_env_bool_accepts_canonical_operator_vocabulary() {
        for raw in ["true", "TRUE", "1", "yes", "YES", "on", " ON "] {
            assert_eq!(
                parse_env_bool(raw),
                Some(true),
                "{raw} should parse to true"
            );
        }
        for raw in ["false", "FALSE", "0", "no", "NO", "off", " OFF "] {
            assert_eq!(
                parse_env_bool(raw),
                Some(false),
                "{raw} should parse to false"
            );
        }
    }

    #[test]
    fn parse_env_bool_rejects_unknown_tokens() {
        for raw in ["maybe", "2", "enabled", "", "  "] {
            assert!(
                parse_env_bool(raw).is_none(),
                "{raw} must not parse as a boolean"
            );
        }
    }

    #[test]
    fn from_environment_with_empty_reader_yields_default_off_config() {
        let unparseable: RefCell<Vec<(&'static str, String)>> = RefCell::new(Vec::new());
        let config = LexicalRamTierConfig::from_environment_with_reader(
            |_name| None,
            |name, raw| unparseable.borrow_mut().push((name, raw.to_owned())),
        );
        assert_eq!(config, LexicalRamTierConfig::default());
        assert!(
            unparseable.borrow().is_empty(),
            "missing values must not trigger on_unparseable: {:?}",
            unparseable.borrow()
        );
    }

    #[test]
    fn from_environment_with_explicit_disable_overrides_default_enabled() {
        let unparseable: RefCell<Vec<(&'static str, String)>> = RefCell::new(Vec::new());
        let reader = env_reader_from(&[(LEXICAL_RAM_TIER_PIN_RAM_ENV, "false")]);
        let config = LexicalRamTierConfig::from_environment_with_reader(reader, |name, raw| {
            unparseable.borrow_mut().push((name, raw.to_owned()))
        });
        assert!(!config.enabled);
        assert!(!config.request_hugepages);
        assert!(
            config.populate_on_open,
            "populate_on_open default should be preserved"
        );
        assert!(unparseable.borrow().is_empty());
    }

    #[test]
    fn from_environment_with_explicit_hugepages_enables_request_only() {
        let unparseable: RefCell<Vec<(&'static str, String)>> = RefCell::new(Vec::new());
        let reader = env_reader_from(&[
            (LEXICAL_RAM_TIER_PIN_RAM_ENV, "1"),
            (LEXICAL_RAM_TIER_HUGEPAGES_ENV, "yes"),
        ]);
        let config = LexicalRamTierConfig::from_environment_with_reader(reader, |name, raw| {
            unparseable.borrow_mut().push((name, raw.to_owned()))
        });
        assert!(config.enabled);
        assert!(config.request_hugepages);
        assert!(unparseable.borrow().is_empty());
    }

    #[test]
    fn from_environment_with_pin_ram_and_hugepages_enables_request() {
        let unparseable: RefCell<Vec<(&'static str, String)>> = RefCell::new(Vec::new());
        let reader = env_reader_from(&[
            (LEXICAL_RAM_TIER_PIN_RAM_ENV, "on"),
            (LEXICAL_RAM_TIER_HUGEPAGES_ENV, "on"),
        ]);
        let config = LexicalRamTierConfig::from_environment_with_reader(reader, |name, raw| {
            unparseable.borrow_mut().push((name, raw.to_owned()))
        });
        assert!(config.enabled);
        assert!(config.request_hugepages);
        assert!(unparseable.borrow().is_empty());
    }

    #[test]
    fn from_environment_records_unparseable_pin_ram_and_keeps_default() {
        let unparseable: RefCell<Vec<(&'static str, String)>> = RefCell::new(Vec::new());
        let reader = env_reader_from(&[(LEXICAL_RAM_TIER_PIN_RAM_ENV, "maybe")]);
        let config = LexicalRamTierConfig::from_environment_with_reader(reader, |name, raw| {
            unparseable.borrow_mut().push((name, raw.to_owned()))
        });
        assert!(
            !config.enabled,
            "unparseable value must leave the default disabled in place"
        );
        let log = unparseable.borrow();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, LEXICAL_RAM_TIER_PIN_RAM_ENV);
        assert_eq!(log[0].1, "maybe");
    }

    #[test]
    fn from_environment_records_unparseable_hugepages_and_keeps_default() {
        let unparseable: RefCell<Vec<(&'static str, String)>> = RefCell::new(Vec::new());
        let reader = env_reader_from(&[(LEXICAL_RAM_TIER_HUGEPAGES_ENV, "kinda")]);
        let config = LexicalRamTierConfig::from_environment_with_reader(reader, |name, raw| {
            unparseable.borrow_mut().push((name, raw.to_owned()))
        });
        assert!(!config.request_hugepages);
        let log = unparseable.borrow();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, LEXICAL_RAM_TIER_HUGEPAGES_ENV);
        assert_eq!(log[0].1, "kinda");
    }

    #[test]
    fn from_environment_forces_hugepages_off_when_pin_ram_is_explicitly_disabled() {
        // Semantic precondition: requesting hugepages on a disabled tier is
        // a no-op at the syscall level. The reader records the inconsistency
        // through on_unparseable so the caller can surface it through the
        // existing lexical_hugepages_unavailable degraded code.
        let unparseable: RefCell<Vec<(&'static str, String)>> = RefCell::new(Vec::new());
        let reader = env_reader_from(&[
            (LEXICAL_RAM_TIER_PIN_RAM_ENV, "false"),
            (LEXICAL_RAM_TIER_HUGEPAGES_ENV, "true"),
        ]);
        let config = LexicalRamTierConfig::from_environment_with_reader(reader, |name, raw| {
            unparseable.borrow_mut().push((name, raw.to_owned()))
        });
        assert!(!config.enabled);
        assert!(
            !config.request_hugepages,
            "hugepages must be forced off when pinning is explicitly disabled"
        );
        let log = unparseable.borrow();
        let inconsistency = log
            .iter()
            .find(|(name, _)| *name == LEXICAL_RAM_TIER_HUGEPAGES_ENV)
            .expect("hugepages inconsistency must be reported through on_unparseable");
        assert_eq!(inconsistency.1, "requires-pin-ram-enabled");
    }

    #[test]
    fn from_environment_pin_ram_default_rejects_hugepages_request_as_no_op() {
        // When pin-ram is not explicitly set the default `enabled=false`
        // wins, so an explicit hugepages opt-in is rejected as a no-op.
        let unparseable: RefCell<Vec<(&'static str, String)>> = RefCell::new(Vec::new());
        let reader = env_reader_from(&[(LEXICAL_RAM_TIER_HUGEPAGES_ENV, "on")]);
        let config = LexicalRamTierConfig::from_environment_with_reader(reader, |name, raw| {
            unparseable.borrow_mut().push((name, raw.to_owned()))
        });
        assert!(!config.enabled);
        assert!(!config.request_hugepages);
        assert_eq!(
            unparseable.borrow().as_slice(),
            &[(
                LEXICAL_RAM_TIER_HUGEPAGES_ENV,
                "requires-pin-ram-enabled".to_owned()
            )]
        );
    }

    #[test]
    fn from_environment_is_deterministic_across_repeated_calls() {
        let reader = env_reader_from(&[
            (LEXICAL_RAM_TIER_PIN_RAM_ENV, "TRUE"),
            (LEXICAL_RAM_TIER_HUGEPAGES_ENV, "ON"),
        ]);
        let first = LexicalRamTierConfig::from_environment_with_reader(&reader, |_, _| {});
        let second = LexicalRamTierConfig::from_environment_with_reader(&reader, |_, _| {});
        assert_eq!(
            first, second,
            "same reader inputs must yield identical configs"
        );
        assert!(first.enabled);
        assert!(first.request_hugepages);
    }

    #[test]
    fn from_environment_env_var_constants_match_bd21xbi_spec() {
        // bd-21xbi spec pins these exact names; pinning them here catches
        // accidental rename drift before the env_registry sibling slice
        // lands.
        assert_eq!(LEXICAL_RAM_TIER_PIN_RAM_ENV, "EE_LEXICAL_INDEX_PIN_RAM");
        assert_eq!(LEXICAL_RAM_TIER_HUGEPAGES_ENV, "EE_LEXICAL_INDEX_HUGEPAGES");
    }
}
