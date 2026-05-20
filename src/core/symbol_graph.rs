use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use crate::models::{
    SYMBOL_EVIDENCE_LINK_ID_PREFIX, SYMBOL_EVIDENCE_LINKS_SCHEMA_V1, SYMBOL_ID_PREFIX,
    SYMBOL_SNAPSHOT_SCHEMA_V1, SymbolEvidenceLink, SymbolEvidenceLinkDegradation,
    SymbolEvidenceLinkDegradationCode, SymbolEvidenceLinkSet, SymbolEvidenceReasonCode,
    SymbolEvidenceResolution, SymbolEvidenceSourceKind, SymbolGraphDegradation,
    SymbolGraphDegradationCode, SymbolGraphDegradationSeverity, SymbolKind, SymbolParserKind,
    SymbolRecord, SymbolSnapshot, SymbolSourceFile, SymbolSourceLanguage, SymbolSourceRange,
    SymbolVisibility,
};

pub const SYMBOL_GRAPH_GENERATOR_V1: &str = "ee.symbol_graph.rust_lexical_scanner.v1";
pub const SYMBOL_EVIDENCE_LINK_GENERATOR_V1: &str = "ee.symbol_graph.evidence_link_resolver.v1";
pub const DEFAULT_MAX_RUST_SOURCE_BYTES: u64 = 1_048_576;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustSourceInput<'a> {
    pub path: Cow<'a, str>,
    pub source: Cow<'a, str>,
}

impl<'a> RustSourceInput<'a> {
    #[must_use]
    pub fn new(path: impl Into<Cow<'a, str>>, source: impl Into<Cow<'a, str>>) -> Self {
        Self {
            path: path.into(),
            source: source.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SymbolExtractorConfig {
    pub max_file_bytes: u64,
}

impl Default for SymbolExtractorConfig {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_RUST_SOURCE_BYTES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolGraphExtractor {
    config: SymbolExtractorConfig,
}

impl Default for SymbolGraphExtractor {
    fn default() -> Self {
        Self::new(SymbolExtractorConfig::default())
    }
}

impl SymbolGraphExtractor {
    #[must_use]
    pub const fn new(config: SymbolExtractorConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn extract_sources(&self, inputs: &[RustSourceInput<'_>]) -> SymbolSnapshot {
        let mut files = Vec::new();
        let mut symbols = Vec::new();
        let mut degraded = Vec::new();

        let mut sorted_inputs: Vec<&RustSourceInput<'_>> = inputs.iter().collect();
        sorted_inputs.sort_by(|left, right| left.path.cmp(&right.path));

        for input in sorted_inputs {
            let path = normalize_path_string(input.path.as_ref());
            let source = input.source.as_ref();

            if source.len() as u64 > self.config.max_file_bytes {
                degraded.push(degradation(
                    SymbolGraphDegradationCode::SourceTooLarge,
                    Some(path),
                    format!(
                        "Rust source is {} bytes, above the {} byte symbol extraction cap.",
                        source.len(),
                        self.config.max_file_bytes
                    ),
                ));
                continue;
            }

            let mut scanner = RustSymbolScanner::new(&path, source);
            let mut file_symbols = scanner.scan();
            degraded.extend(scanner.degraded);
            file_symbols.sort_by(compare_symbols);

            files.push(SymbolSourceFile {
                path: path.clone(),
                language: SymbolSourceLanguage::Rust,
                parser: SymbolParserKind::RustLexicalScanner,
                source_hash: blake3_hex(source.as_bytes()),
                byte_len: source.len() as u64,
                symbol_count: file_symbols.len(),
            });
            symbols.extend(file_symbols);
        }

        finish_snapshot(None, files, symbols, degraded)
    }

    #[must_use]
    pub fn extract_paths(
        &self,
        workspace_root: impl AsRef<Path>,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> SymbolSnapshot {
        let workspace_root = workspace_root.as_ref();
        let mut files = Vec::new();
        let mut symbols = Vec::new();
        let mut degraded = Vec::new();
        let mut sorted_paths: Vec<PathBuf> = paths.into_iter().collect();
        sorted_paths.sort_by_key(|path| normalize_path_for_order(path));

        for path in sorted_paths {
            let display_path = workspace_relative_path(workspace_root, &path);
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    degraded.push(degradation(
                        SymbolGraphDegradationCode::SourceMissing,
                        Some(display_path),
                        format!("Rust source file is missing: {}", path.display()),
                    ));
                    continue;
                }
                Err(error) => {
                    degraded.push(degradation(
                        SymbolGraphDegradationCode::SourceUnreadable,
                        Some(display_path),
                        format!("Rust source metadata could not be read: {error}"),
                    ));
                    continue;
                }
            };

            if !metadata.is_file() {
                degraded.push(degradation(
                    SymbolGraphDegradationCode::SourceNonRegular,
                    Some(display_path),
                    format!("Rust source path is not a regular file: {}", path.display()),
                ));
                continue;
            }

            if metadata.len() > self.config.max_file_bytes {
                degraded.push(degradation(
                    SymbolGraphDegradationCode::SourceTooLarge,
                    Some(display_path),
                    format!(
                        "Rust source is {} bytes, above the {} byte symbol extraction cap.",
                        metadata.len(),
                        self.config.max_file_bytes
                    ),
                ));
                continue;
            }

            let source = match fs::read_to_string(&path) {
                Ok(source) => source,
                Err(error) => {
                    degraded.push(degradation(
                        SymbolGraphDegradationCode::SourceUnreadable,
                        Some(display_path),
                        format!("Rust source could not be read as UTF-8 text: {error}"),
                    ));
                    continue;
                }
            };

            let mut scanner = RustSymbolScanner::new(&display_path, &source);
            let mut file_symbols = scanner.scan();
            degraded.extend(scanner.degraded);
            file_symbols.sort_by(compare_symbols);

            files.push(SymbolSourceFile {
                path: display_path.clone(),
                language: SymbolSourceLanguage::Rust,
                parser: SymbolParserKind::RustLexicalScanner,
                source_hash: blake3_hex(source.as_bytes()),
                byte_len: source.len() as u64,
                symbol_count: file_symbols.len(),
            });
            symbols.extend(file_symbols);
        }

        finish_snapshot(
            Some(normalize_path_string(&workspace_root.to_string_lossy())),
            files,
            symbols,
            degraded,
        )
    }
}

#[must_use]
pub fn extract_rust_symbol_snapshot_from_sources(inputs: &[RustSourceInput<'_>]) -> SymbolSnapshot {
    SymbolGraphExtractor::default().extract_sources(inputs)
}

#[derive(Clone, Debug, PartialEq)]
pub struct SymbolEvidenceInput<'a> {
    pub source_kind: SymbolEvidenceSourceKind,
    pub evidence_id: Cow<'a, str>,
    pub provenance_uri: Cow<'a, str>,
    pub target_path: Cow<'a, str>,
    pub start_line: u32,
    pub end_line: u32,
    pub confidence: f32,
    pub expected_symbol_id: Option<Cow<'a, str>>,
    pub expected_rename_fingerprint: Option<Cow<'a, str>>,
}

impl<'a> SymbolEvidenceInput<'a> {
    #[must_use]
    pub fn new(
        source_kind: SymbolEvidenceSourceKind,
        evidence_id: impl Into<Cow<'a, str>>,
        provenance_uri: impl Into<Cow<'a, str>>,
        target_path: impl Into<Cow<'a, str>>,
        start_line: u32,
        end_line: u32,
        confidence: f32,
    ) -> Self {
        Self {
            source_kind,
            evidence_id: evidence_id.into(),
            provenance_uri: provenance_uri.into(),
            target_path: target_path.into(),
            start_line,
            end_line,
            confidence,
            expected_symbol_id: None,
            expected_rename_fingerprint: None,
        }
    }

    #[must_use]
    pub fn with_expected_symbol(
        mut self,
        symbol_id: impl Into<Cow<'a, str>>,
        rename_fingerprint: impl Into<Cow<'a, str>>,
    ) -> Self {
        self.expected_symbol_id = Some(symbol_id.into());
        self.expected_rename_fingerprint = Some(rename_fingerprint.into());
        self
    }
}

#[must_use]
pub fn link_symbol_evidence(
    snapshot: &SymbolSnapshot,
    inputs: &[SymbolEvidenceInput<'_>],
) -> SymbolEvidenceLinkSet {
    let mut sorted_inputs: Vec<&SymbolEvidenceInput<'_>> = inputs.iter().collect();
    sorted_inputs.sort_by(|left, right| {
        (
            left.source_kind,
            left.evidence_id.as_ref(),
            left.provenance_uri.as_ref(),
            left.target_path.as_ref(),
            left.start_line,
            left.end_line,
        )
            .cmp(&(
                right.source_kind,
                right.evidence_id.as_ref(),
                right.provenance_uri.as_ref(),
                right.target_path.as_ref(),
                right.start_line,
                right.end_line,
            ))
    });

    let mut symbols_by_path: BTreeMap<&str, Vec<&SymbolRecord>> = BTreeMap::new();
    let mut symbols_by_id: BTreeMap<&str, &SymbolRecord> = BTreeMap::new();
    for symbol in &snapshot.symbols {
        symbols_by_path
            .entry(symbol.path.as_str())
            .or_default()
            .push(symbol);
        symbols_by_id.insert(symbol.id.as_str(), symbol);
    }
    for symbols in symbols_by_path.values_mut() {
        symbols.sort_by(compare_symbol_refs);
    }

    let mut links = Vec::with_capacity(sorted_inputs.len());
    let mut degraded = Vec::new();
    for input in sorted_inputs {
        let (link, maybe_degraded) =
            resolve_symbol_evidence_input(snapshot, input, &symbols_by_path, &symbols_by_id);
        if let Some(item) = maybe_degraded {
            degraded.push(item);
        }
        links.push(link);
    }

    degraded.sort_by(|left, right| {
        (
            left.evidence_id.as_str(),
            left.path.as_deref().unwrap_or(""),
            left.code,
            left.message.as_str(),
        )
            .cmp(&(
                right.evidence_id.as_str(),
                right.path.as_deref().unwrap_or(""),
                right.code,
                right.message.as_str(),
            ))
    });

    SymbolEvidenceLinkSet {
        schema: SYMBOL_EVIDENCE_LINKS_SCHEMA_V1.to_string(),
        snapshot_hash: snapshot.snapshot_hash.clone(),
        generated_by: SYMBOL_EVIDENCE_LINK_GENERATOR_V1.to_string(),
        source_manifest_hash: symbol_evidence_source_manifest_hash(snapshot, &sorted_inputs),
        links,
        degraded,
    }
}

fn resolve_symbol_evidence_input(
    snapshot: &SymbolSnapshot,
    input: &SymbolEvidenceInput<'_>,
    symbols_by_path: &BTreeMap<&str, Vec<&SymbolRecord>>,
    symbols_by_id: &BTreeMap<&str, &SymbolRecord>,
) -> (SymbolEvidenceLink, Option<SymbolEvidenceLinkDegradation>) {
    let target_path = normalize_path_string(input.target_path.as_ref());
    let target_range = evidence_target_range(input.start_line, input.end_line);
    let confidence = normalized_confidence(input.confidence);

    if input.start_line == 0 || input.end_line == 0 || input.end_line < input.start_line {
        return link_without_symbol(
            input,
            target_path,
            target_range,
            0.0,
            SymbolEvidenceResolution::StaleSpan,
            SymbolEvidenceReasonCode::StaleLineSpan,
            Some(SymbolEvidenceLinkDegradationCode::StaleLineSpan),
            "evidence line span is stale or invalid",
        );
    }

    if let Some(expected_id) = input.expected_symbol_id.as_ref() {
        if let Some(symbol) = symbols_by_id.get(expected_id.as_ref()) {
            return link_with_symbol(
                input,
                &target_path,
                target_range,
                symbol,
                confidence,
                SymbolEvidenceResolution::ExactSymbol,
                SymbolEvidenceReasonCode::ExactSymbolSpan,
            );
        }
        if let Some(rename_fingerprint) = input.expected_rename_fingerprint.as_ref() {
            let renamed: Vec<&SymbolRecord> = snapshot
                .symbols
                .iter()
                .filter(|symbol| {
                    symbol.path == target_path
                        && symbol.rename_fingerprint == rename_fingerprint.as_ref()
                })
                .collect();
            if renamed.len() == 1 {
                return (
                    build_symbol_link(
                        input,
                        target_path,
                        target_range,
                        Some(renamed[0]),
                        scaled_confidence(confidence, 0.85),
                        SymbolEvidenceResolution::RenamedSymbol,
                        SymbolEvidenceReasonCode::SymbolRenamedByFingerprint,
                    ),
                    Some(link_degradation(
                        SymbolEvidenceLinkDegradationCode::SymbolRenamed,
                        input,
                        Some(normalize_path_string(input.target_path.as_ref())),
                        "expected symbol id was absent but rename fingerprint matched a current symbol",
                    )),
                );
            }
        }
        return link_without_symbol(
            input,
            target_path,
            target_range,
            0.0,
            SymbolEvidenceResolution::DeletedSymbol,
            SymbolEvidenceReasonCode::SymbolDeleted,
            Some(SymbolEvidenceLinkDegradationCode::SymbolDeleted),
            "expected symbol id was absent from the current snapshot",
        );
    }

    let Some(symbols) = symbols_by_path.get(target_path.as_str()) else {
        return link_without_symbol(
            input,
            target_path,
            target_range,
            0.0,
            SymbolEvidenceResolution::SourceFileMissing,
            SymbolEvidenceReasonCode::SourceFileMissing,
            Some(SymbolEvidenceLinkDegradationCode::SourceFileMissing),
            "evidence target file is absent from the current symbol snapshot",
        );
    };

    let mut candidates: Vec<&SymbolRecord> = symbols
        .iter()
        .copied()
        .filter(|symbol| symbol_contains_lines(symbol, input.start_line, input.end_line))
        .collect();
    candidates.sort_by(|left, right| {
        (
            containment_score(left, input.start_line, input.end_line),
            symbol_line_width(left),
            left.canonical_name.as_str(),
            left.id.as_str(),
        )
            .cmp(&(
                containment_score(right, input.start_line, input.end_line),
                symbol_line_width(right),
                right.canonical_name.as_str(),
                right.id.as_str(),
            ))
    });

    match candidates.as_slice() {
        [] => (
            build_symbol_link(
                input,
                target_path,
                target_range,
                None,
                scaled_confidence(confidence, 0.40),
                SymbolEvidenceResolution::FileLevel,
                SymbolEvidenceReasonCode::FileLevelNoContainingSymbol,
            ),
            None,
        ),
        [symbol] => {
            let exact = symbol.range.start_line == input.start_line
                && symbol.range.end_line == input.end_line;
            let (resolution, reason) = if exact {
                (
                    SymbolEvidenceResolution::ExactSymbol,
                    SymbolEvidenceReasonCode::ExactSymbolSpan,
                )
            } else {
                (
                    SymbolEvidenceResolution::ContainingSymbol,
                    SymbolEvidenceReasonCode::ContainingSymbolSpan,
                )
            };
            link_with_symbol(
                input,
                &target_path,
                target_range,
                symbol,
                confidence,
                resolution,
                reason,
            )
        }
        [first, second, ..]
            if containment_score(first, input.start_line, input.end_line)
                == containment_score(second, input.start_line, input.end_line)
                && symbol_line_width(first) == symbol_line_width(second) =>
        {
            link_without_symbol(
                input,
                target_path,
                target_range,
                scaled_confidence(confidence, 0.25),
                SymbolEvidenceResolution::Ambiguous,
                SymbolEvidenceReasonCode::AmbiguousContainingSymbols,
                Some(SymbolEvidenceLinkDegradationCode::AmbiguousContainingSymbols),
                "multiple symbols matched the evidence span with equal specificity",
            )
        }
        [symbol, ..] => link_with_symbol(
            input,
            &target_path,
            target_range,
            symbol,
            confidence,
            SymbolEvidenceResolution::ContainingSymbol,
            SymbolEvidenceReasonCode::ContainingSymbolSpan,
        ),
    }
}

fn link_with_symbol(
    input: &SymbolEvidenceInput<'_>,
    target_path: &str,
    target_range: SymbolSourceRange,
    symbol: &SymbolRecord,
    confidence: f32,
    resolution: SymbolEvidenceResolution,
    reason: SymbolEvidenceReasonCode,
) -> (SymbolEvidenceLink, Option<SymbolEvidenceLinkDegradation>) {
    (
        build_symbol_link(
            input,
            target_path.to_owned(),
            target_range,
            Some(symbol),
            confidence,
            resolution,
            reason,
        ),
        None,
    )
}

fn link_without_symbol(
    input: &SymbolEvidenceInput<'_>,
    target_path: String,
    target_range: SymbolSourceRange,
    confidence: f32,
    resolution: SymbolEvidenceResolution,
    reason: SymbolEvidenceReasonCode,
    degraded_code: Option<SymbolEvidenceLinkDegradationCode>,
    message: &'static str,
) -> (SymbolEvidenceLink, Option<SymbolEvidenceLinkDegradation>) {
    let degraded =
        degraded_code.map(|code| link_degradation(code, input, Some(target_path.clone()), message));
    (
        build_symbol_link(
            input,
            target_path,
            target_range,
            None,
            confidence,
            resolution,
            reason,
        ),
        degraded,
    )
}

fn build_symbol_link(
    input: &SymbolEvidenceInput<'_>,
    target_path: String,
    target_range: SymbolSourceRange,
    symbol: Option<&SymbolRecord>,
    confidence: f32,
    resolution: SymbolEvidenceResolution,
    reason: SymbolEvidenceReasonCode,
) -> SymbolEvidenceLink {
    SymbolEvidenceLink {
        link_id: symbol_evidence_link_id(input, &target_path, symbol, resolution, reason),
        source_kind: input.source_kind,
        evidence_id: input.evidence_id.to_string(),
        provenance_uri: input.provenance_uri.to_string(),
        target_path,
        target_range,
        symbol_id: symbol.map(|symbol| symbol.id.clone()),
        canonical_name: symbol.map(|symbol| symbol.canonical_name.clone()),
        symbol_kind: symbol.map(|symbol| symbol.kind),
        symbol_range: symbol.map(|symbol| symbol.range),
        confidence: normalized_confidence(confidence),
        resolution,
        reason,
    }
}

fn link_degradation(
    code: SymbolEvidenceLinkDegradationCode,
    input: &SymbolEvidenceInput<'_>,
    path: Option<String>,
    message: &'static str,
) -> SymbolEvidenceLinkDegradation {
    SymbolEvidenceLinkDegradation {
        code,
        severity: SymbolGraphDegradationSeverity::Warning,
        evidence_id: input.evidence_id.to_string(),
        path,
        message: message.to_owned(),
    }
}

fn evidence_target_range(start_line: u32, end_line: u32) -> SymbolSourceRange {
    let safe_start = start_line.max(1);
    let safe_end = end_line.max(safe_start);
    SymbolSourceRange {
        start_line: safe_start,
        start_column: 1,
        end_line: safe_end,
        end_column: 1,
    }
}

fn symbol_contains_lines(symbol: &SymbolRecord, start_line: u32, end_line: u32) -> bool {
    symbol.range.start_line <= start_line && symbol.range.end_line >= end_line
}

fn containment_score(symbol: &SymbolRecord, start_line: u32, end_line: u32) -> u32 {
    start_line
        .saturating_sub(symbol.range.start_line)
        .saturating_add(symbol.range.end_line.saturating_sub(end_line))
}

fn symbol_line_width(symbol: &SymbolRecord) -> u32 {
    symbol
        .range
        .end_line
        .saturating_sub(symbol.range.start_line)
        .saturating_add(1)
}

fn compare_symbol_refs(left: &&SymbolRecord, right: &&SymbolRecord) -> std::cmp::Ordering {
    compare_symbols(left, right)
}

fn normalized_confidence(value: f32) -> f32 {
    if value.is_finite() {
        (value.clamp(0.0, 1.0) * 1000.0).round() / 1000.0
    } else {
        0.0
    }
}

fn scaled_confidence(value: f32, scale: f32) -> f32 {
    normalized_confidence(normalized_confidence(value) * scale)
}

fn symbol_evidence_link_id(
    input: &SymbolEvidenceInput<'_>,
    target_path: &str,
    symbol: Option<&SymbolRecord>,
    resolution: SymbolEvidenceResolution,
    reason: SymbolEvidenceReasonCode,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, b"ee.symbol_evidence_link.v1");
    hash_part(&mut hasher, input.source_kind.as_str().as_bytes());
    hash_part(&mut hasher, input.evidence_id.as_ref().as_bytes());
    hash_part(&mut hasher, input.provenance_uri.as_ref().as_bytes());
    hash_part(&mut hasher, target_path.as_bytes());
    hash_part(&mut hasher, input.start_line.to_string().as_bytes());
    hash_part(&mut hasher, input.end_line.to_string().as_bytes());
    hash_part(
        &mut hasher,
        symbol.map_or("", |symbol| symbol.id.as_str()).as_bytes(),
    );
    hash_part(&mut hasher, resolution.as_str().as_bytes());
    hash_part(&mut hasher, reason.as_str().as_bytes());
    format!(
        "{}{}",
        SYMBOL_EVIDENCE_LINK_ID_PREFIX,
        &hasher.finalize().to_hex()[..24]
    )
}

fn symbol_evidence_source_manifest_hash(
    snapshot: &SymbolSnapshot,
    inputs: &[&SymbolEvidenceInput<'_>],
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, b"ee.symbol_evidence_manifest.v1");
    hash_part(&mut hasher, snapshot.snapshot_hash.as_bytes());
    for input in inputs {
        hash_part(&mut hasher, input.source_kind.as_str().as_bytes());
        hash_part(&mut hasher, input.evidence_id.as_ref().as_bytes());
        hash_part(&mut hasher, input.provenance_uri.as_ref().as_bytes());
        hash_part(
            &mut hasher,
            normalize_path_string(input.target_path.as_ref()).as_bytes(),
        );
        hash_part(&mut hasher, input.start_line.to_string().as_bytes());
        hash_part(&mut hasher, input.end_line.to_string().as_bytes());
        hash_part(
            &mut hasher,
            normalized_confidence(input.confidence)
                .to_string()
                .as_bytes(),
        );
        hash_part(
            &mut hasher,
            input.expected_symbol_id.as_deref().unwrap_or("").as_bytes(),
        );
        hash_part(
            &mut hasher,
            input
                .expected_rename_fingerprint
                .as_deref()
                .unwrap_or("")
                .as_bytes(),
        );
    }
    hasher.finalize().to_hex().to_string()
}

fn finish_snapshot(
    workspace_root: Option<String>,
    mut files: Vec<SymbolSourceFile>,
    mut symbols: Vec<SymbolRecord>,
    mut degraded: Vec<SymbolGraphDegradation>,
) -> SymbolSnapshot {
    files.sort_by(|left, right| left.path.cmp(&right.path));
    symbols.sort_by(compare_symbols);
    degraded.sort_by(|left, right| {
        (
            left.path.as_deref().unwrap_or(""),
            left.code,
            left.message.as_str(),
        )
            .cmp(&(
                right.path.as_deref().unwrap_or(""),
                right.code,
                right.message.as_str(),
            ))
    });

    let mut snapshot = SymbolSnapshot {
        schema: SYMBOL_SNAPSHOT_SCHEMA_V1.to_string(),
        workspace_root,
        generated_by: SYMBOL_GRAPH_GENERATOR_V1.to_string(),
        files,
        symbols,
        degraded,
        snapshot_hash: String::new(),
    };
    snapshot.snapshot_hash = snapshot_hash(&snapshot);
    snapshot
}

fn snapshot_hash(snapshot: &SymbolSnapshot) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, snapshot.schema.as_bytes());
    hash_part(
        &mut hasher,
        snapshot.workspace_root.as_deref().unwrap_or("").as_bytes(),
    );
    hash_part(&mut hasher, snapshot.generated_by.as_bytes());

    for file in &snapshot.files {
        hash_part(&mut hasher, file.path.as_bytes());
        hash_part(&mut hasher, format!("{:?}", file.language).as_bytes());
        hash_part(&mut hasher, format!("{:?}", file.parser).as_bytes());
        hash_part(&mut hasher, file.source_hash.as_bytes());
        hash_part(&mut hasher, file.byte_len.to_string().as_bytes());
        hash_part(&mut hasher, file.symbol_count.to_string().as_bytes());
    }

    for symbol in &snapshot.symbols {
        hash_part(&mut hasher, symbol.id.as_bytes());
        hash_part(&mut hasher, symbol.kind.as_str().as_bytes());
        hash_part(&mut hasher, symbol.canonical_name.as_bytes());
        hash_part(&mut hasher, symbol.namespace.join("::").as_bytes());
        hash_part(&mut hasher, symbol.path.as_bytes());
        hash_part(&mut hasher, format!("{:?}", symbol.range).as_bytes());
        hash_part(&mut hasher, format!("{:?}", symbol.visibility).as_bytes());
        hash_part(&mut hasher, symbol.declaration_hash.as_bytes());
        hash_part(&mut hasher, symbol.rename_fingerprint.as_bytes());
        hash_part(&mut hasher, format!("{:?}", symbol.parser).as_bytes());
    }

    for item in &snapshot.degraded {
        hash_part(&mut hasher, format!("{:?}", item.code).as_bytes());
        hash_part(&mut hasher, format!("{:?}", item.severity).as_bytes());
        hash_part(&mut hasher, item.path.as_deref().unwrap_or("").as_bytes());
        hash_part(&mut hasher, item.message.as_bytes());
    }

    hasher.finalize().to_hex().to_string()
}

fn hash_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(bytes);
    hasher.update(b"\0");
}

fn compare_symbols(left: &SymbolRecord, right: &SymbolRecord) -> std::cmp::Ordering {
    (
        left.path.as_str(),
        left.range,
        left.kind,
        left.canonical_name.as_str(),
    )
        .cmp(&(
            right.path.as_str(),
            right.range,
            right.kind,
            right.canonical_name.as_str(),
        ))
}

fn degradation(
    code: SymbolGraphDegradationCode,
    path: Option<String>,
    message: String,
) -> SymbolGraphDegradation {
    let severity = match code {
        SymbolGraphDegradationCode::SourceUnparsable => SymbolGraphDegradationSeverity::Medium,
        SymbolGraphDegradationCode::SourceMissing
        | SymbolGraphDegradationCode::SourceNonRegular
        | SymbolGraphDegradationCode::SourceTooLarge
        | SymbolGraphDegradationCode::SourceUnreadable
        | SymbolGraphDegradationCode::SymbolIndexStale => SymbolGraphDegradationSeverity::Warning,
    };

    SymbolGraphDegradation {
        code,
        severity,
        path,
        message,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TokenKind {
    Ident,
    Literal,
    Punct,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Token {
    text: String,
    kind: TokenKind,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContainerKind {
    Module,
    Trait,
    Impl,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DeclarationBoundary {
    signature_end: usize,
    end_idx: usize,
    body: Option<(usize, usize)>,
}

struct SymbolAddInput<'a> {
    kind: SymbolKind,
    start_idx: usize,
    name_idx: Option<usize>,
    local_name: &'a str,
    boundary: DeclarationBoundary,
    namespace: &'a [String],
    visibility: SymbolVisibility,
}

struct RustSymbolScanner<'a> {
    path: &'a str,
    source: &'a str,
    line_starts: Vec<usize>,
    tokens: Vec<Token>,
    brace_pairs: HashMap<usize, usize>,
    degraded: Vec<SymbolGraphDegradation>,
    symbols: Vec<SymbolRecord>,
}

impl<'a> RustSymbolScanner<'a> {
    fn new(path: &'a str, source: &'a str) -> Self {
        let line_starts = line_starts(source);
        let tokens = lex_rust(source);
        let (brace_pairs, degraded) = brace_pairs(path, &tokens);
        Self {
            path,
            source,
            line_starts,
            tokens,
            brace_pairs,
            degraded,
            symbols: Vec::new(),
        }
    }

    fn scan(&mut self) -> Vec<SymbolRecord> {
        self.parse_block(0, self.tokens.len(), &[], ContainerKind::Module);
        std::mem::take(&mut self.symbols)
    }

    fn parse_block(
        &mut self,
        start: usize,
        end: usize,
        namespace: &[String],
        container: ContainerKind,
    ) {
        let mut idx = start;
        while idx < end {
            let text = self.tokens[idx].text.clone();
            match text.as_str() {
                "mod" => {
                    if let Some(name_idx) = self.next_ident(idx + 1, end) {
                        let boundary = self.declaration_boundary(idx, end);
                        self.add_named_symbol(
                            SymbolKind::Module,
                            idx,
                            name_idx,
                            boundary,
                            namespace,
                            self.visibility_before(idx),
                        );
                        if let Some((body_start, body_end)) = boundary.body {
                            let mut child_namespace = namespace.to_vec();
                            child_namespace.push(self.tokens[name_idx].text.clone());
                            self.parse_block(
                                body_start + 1,
                                body_end,
                                &child_namespace,
                                ContainerKind::Module,
                            );
                        }
                        idx = boundary.end_idx.saturating_add(1);
                        continue;
                    }
                }
                "struct" | "enum" | "trait" => {
                    if let Some(name_idx) = self.next_ident(idx + 1, end) {
                        let kind = match text.as_str() {
                            "struct" => SymbolKind::Struct,
                            "enum" => SymbolKind::Enum,
                            "trait" => SymbolKind::Trait,
                            _ => unreachable!(),
                        };
                        let boundary = self.declaration_boundary(idx, end);
                        self.add_named_symbol(
                            kind,
                            idx,
                            name_idx,
                            boundary,
                            namespace,
                            self.visibility_before(idx),
                        );
                        if kind == SymbolKind::Trait {
                            if let Some((body_start, body_end)) = boundary.body {
                                let mut child_namespace = namespace.to_vec();
                                child_namespace.push(self.tokens[name_idx].text.clone());
                                self.parse_block(
                                    body_start + 1,
                                    body_end,
                                    &child_namespace,
                                    ContainerKind::Trait,
                                );
                            }
                        }
                        idx = boundary.end_idx.saturating_add(1);
                        continue;
                    }
                }
                "impl" => {
                    let boundary = self.declaration_boundary(idx, end);
                    let impl_name = self.impl_name(idx, boundary.signature_end);
                    self.add_symbol(SymbolAddInput {
                        kind: SymbolKind::Impl,
                        start_idx: idx,
                        name_idx: None,
                        local_name: &impl_name,
                        boundary,
                        namespace,
                        visibility: SymbolVisibility::Private,
                    });
                    if let Some((body_start, body_end)) = boundary.body {
                        let mut child_namespace = namespace.to_vec();
                        child_namespace.push(impl_name);
                        self.parse_block(
                            body_start + 1,
                            body_end,
                            &child_namespace,
                            ContainerKind::Impl,
                        );
                    }
                    idx = boundary.end_idx.saturating_add(1);
                    continue;
                }
                "fn" => {
                    if let Some(name_idx) = self.next_ident(idx + 1, end) {
                        let name = self.tokens[name_idx].text.as_str();
                        let kind = match container {
                            ContainerKind::Trait | ContainerKind::Impl => SymbolKind::Method,
                            ContainerKind::Module if is_cli_command_handler(name, namespace) => {
                                SymbolKind::CliCommandHandler
                            }
                            ContainerKind::Module => SymbolKind::Function,
                        };
                        let boundary = self.declaration_boundary(idx, end);
                        let visibility = if container == ContainerKind::Trait {
                            SymbolVisibility::Public
                        } else {
                            self.visibility_before(idx)
                        };
                        self.add_named_symbol(kind, idx, name_idx, boundary, namespace, visibility);
                        idx = boundary.end_idx.saturating_add(1);
                        continue;
                    }
                }
                "const" => {
                    if let Some(name_idx) = self.next_ident(idx + 1, end) {
                        let name = self.tokens[name_idx].text.as_str();
                        if is_json_schema_constant(name) {
                            let boundary = self.declaration_boundary(idx, end);
                            self.add_named_symbol(
                                SymbolKind::JsonSchemaConstant,
                                idx,
                                name_idx,
                                boundary,
                                namespace,
                                self.visibility_before(idx),
                            );
                            idx = boundary.end_idx.saturating_add(1);
                            continue;
                        }
                    }
                }
                _ => {
                    if self.is_macro_invocation(idx, end) {
                        let boundary = self.macro_boundary(idx, end);
                        let name = self.macro_name(idx, end);
                        self.add_symbol(SymbolAddInput {
                            kind: SymbolKind::MacroInvocation,
                            start_idx: idx,
                            name_idx: Some(idx),
                            local_name: &name,
                            boundary,
                            namespace,
                            visibility: self.visibility_before(idx),
                        });
                        idx = boundary.end_idx.saturating_add(1);
                        continue;
                    }
                }
            }

            idx += 1;
        }
    }

    fn add_named_symbol(
        &mut self,
        kind: SymbolKind,
        start_idx: usize,
        name_idx: usize,
        boundary: DeclarationBoundary,
        namespace: &[String],
        visibility: SymbolVisibility,
    ) {
        let name = self.tokens[name_idx].text.clone();
        self.add_symbol(SymbolAddInput {
            kind,
            start_idx,
            name_idx: Some(name_idx),
            local_name: &name,
            boundary,
            namespace,
            visibility,
        });
    }

    fn add_symbol(&mut self, input: SymbolAddInput<'_>) {
        let SymbolAddInput {
            kind,
            start_idx,
            name_idx,
            local_name,
            boundary,
            namespace,
            visibility,
        } = input;

        if self.tokens.is_empty() || start_idx >= self.tokens.len() {
            return;
        }

        let end_idx = boundary.end_idx.min(self.tokens.len().saturating_sub(1));
        let start_byte = self.tokens[start_idx].start;
        let end_byte = self.tokens[end_idx].end.min(self.source.len());
        let namespace_vec = namespace.to_vec();
        let canonical_name = canonical_name(namespace, local_name);
        let declaration_hash = blake3_hex(&self.source.as_bytes()[start_byte..end_byte]);
        let signature_shape = self.signature_shape(start_idx, boundary.signature_end, name_idx);
        let rename_fingerprint = rename_fingerprint(kind, namespace, &signature_shape);
        let id = symbol_id(
            self.path,
            namespace,
            kind,
            &canonical_name,
            &rename_fingerprint,
        );

        self.symbols.push(SymbolRecord {
            id,
            kind,
            canonical_name,
            namespace: namespace_vec,
            path: self.path.to_string(),
            range: self.source_range(start_byte, end_byte),
            visibility,
            declaration_hash,
            rename_fingerprint,
            parser: SymbolParserKind::RustLexicalScanner,
        });
    }

    fn declaration_boundary(&mut self, keyword_idx: usize, end: usize) -> DeclarationBoundary {
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut idx = keyword_idx + 1;
        while idx < end {
            match self.tokens[idx].text.as_str() {
                "(" => paren_depth = paren_depth.saturating_add(1),
                ")" => paren_depth = paren_depth.saturating_sub(1),
                "[" => bracket_depth = bracket_depth.saturating_add(1),
                "]" => bracket_depth = bracket_depth.saturating_sub(1),
                "{" if paren_depth == 0 && bracket_depth == 0 => {
                    if let Some(close_idx) = self.brace_pairs.get(&idx).copied() {
                        return DeclarationBoundary {
                            signature_end: idx,
                            end_idx: close_idx,
                            body: Some((idx, close_idx)),
                        };
                    }
                    self.push_unparsable("opening brace has no matching closing brace");
                    return DeclarationBoundary {
                        signature_end: idx,
                        end_idx: idx,
                        body: None,
                    };
                }
                ";" if paren_depth == 0 && bracket_depth == 0 => {
                    return DeclarationBoundary {
                        signature_end: idx,
                        end_idx: idx,
                        body: None,
                    };
                }
                _ => {}
            }
            idx += 1;
        }

        self.push_unparsable("declaration did not terminate before the block ended");
        let end_idx = end.saturating_sub(1).max(keyword_idx);
        DeclarationBoundary {
            signature_end: end_idx.saturating_add(1),
            end_idx,
            body: None,
        }
    }

    fn macro_boundary(&mut self, start_idx: usize, end: usize) -> DeclarationBoundary {
        let bang_idx = start_idx + 1;
        let group_idx = if self.tokens[start_idx].text == "macro_rules"
            && start_idx + 2 < end
            && self.tokens[start_idx + 2].kind == TokenKind::Ident
        {
            start_idx + 3
        } else {
            bang_idx + 1
        };
        let mut end_idx = bang_idx.min(self.tokens.len().saturating_sub(1));
        if let Some(group_start) = self.tokens.get(group_idx) {
            if matches!(group_start.text.as_str(), "{" | "(" | "[") {
                if group_start.text == "{" {
                    if let Some(close_idx) = self.brace_pairs.get(&group_idx).copied() {
                        end_idx = close_idx;
                    } else {
                        self.push_unparsable("macro invocation brace group is unbalanced");
                    }
                } else {
                    end_idx = self.find_group_end(group_idx, end);
                }
            } else if group_idx < end {
                end_idx = group_idx;
            }
        }
        if self
            .tokens
            .get(end_idx + 1)
            .is_some_and(|token| token.text == ";")
        {
            end_idx += 1;
        }
        DeclarationBoundary {
            signature_end: group_idx.min(end),
            end_idx,
            body: None,
        }
    }

    fn find_group_end(&self, start_idx: usize, end: usize) -> usize {
        let (open, close) = match self.tokens[start_idx].text.as_str() {
            "(" => ("(", ")"),
            "[" => ("[", "]"),
            _ => return start_idx,
        };
        let mut depth = 0usize;
        for idx in start_idx..end {
            if self.tokens[idx].text == open {
                depth = depth.saturating_add(1);
            } else if self.tokens[idx].text == close {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return idx;
                }
            }
        }
        start_idx
    }

    fn is_macro_invocation(&self, idx: usize, end: usize) -> bool {
        if idx + 1 >= end {
            return false;
        }
        self.tokens[idx].kind == TokenKind::Ident
            && self.tokens[idx + 1].text == "!"
            && self
                .tokens
                .get(idx.wrapping_sub(1))
                .is_none_or(|token| token.text != "#")
    }

    fn macro_name(&self, idx: usize, end: usize) -> String {
        if self.tokens[idx].text == "macro_rules"
            && idx + 2 < end
            && self.tokens[idx + 2].kind == TokenKind::Ident
        {
            format!("macro_rules! {}", self.tokens[idx + 2].text)
        } else {
            format!("{}!", self.tokens[idx].text)
        }
    }

    fn impl_name(&self, start_idx: usize, signature_end: usize) -> String {
        let shape = self.signature_shape(start_idx, signature_end, None);
        shape
            .split_whitespace()
            .collect::<Vec<&str>>()
            .join(" ")
            .trim()
            .to_string()
    }

    fn next_ident(&self, start: usize, end: usize) -> Option<usize> {
        (start..end).find(|idx| self.tokens[*idx].kind == TokenKind::Ident)
    }

    fn visibility_before(&self, idx: usize) -> SymbolVisibility {
        let start = idx.saturating_sub(8);
        for lookback in (start..idx).rev() {
            match self.tokens[lookback].text.as_str() {
                "pub" => {
                    if self
                        .tokens
                        .get(lookback + 1)
                        .is_some_and(|token| token.text == "(")
                    {
                        return SymbolVisibility::Restricted;
                    }
                    return SymbolVisibility::Public;
                }
                "{" | "}" | ";" => return SymbolVisibility::Private,
                _ => {}
            }
        }
        SymbolVisibility::Private
    }

    fn signature_shape(
        &self,
        start_idx: usize,
        signature_end: usize,
        name_idx: Option<usize>,
    ) -> String {
        let safe_end = signature_end.min(self.tokens.len());
        let mut parts = Vec::new();
        for idx in start_idx..safe_end {
            if Some(idx) == name_idx {
                parts.push("_".to_string());
            } else if self.tokens[idx].kind == TokenKind::Literal {
                parts.push("<literal>".to_string());
            } else {
                parts.push(self.tokens[idx].text.clone());
            }
        }
        parts.join(" ")
    }

    fn source_range(&self, start_byte: usize, end_byte: usize) -> SymbolSourceRange {
        let (start_line, start_column) = byte_position(&self.line_starts, start_byte);
        let (end_line, end_column) = byte_position(&self.line_starts, end_byte);
        SymbolSourceRange {
            start_line,
            start_column,
            end_line,
            end_column,
        }
    }

    fn push_unparsable(&mut self, message: &str) {
        self.degraded.push(degradation(
            SymbolGraphDegradationCode::SourceUnparsable,
            Some(self.path.to_string()),
            message.to_string(),
        ));
    }
}

fn lex_rust(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut idx = 0usize;

    while idx < bytes.len() {
        let byte = bytes[idx];
        if byte.is_ascii_whitespace() {
            idx += 1;
            continue;
        }

        if bytes.get(idx..idx + 2) == Some(b"//") {
            idx = skip_line_comment(bytes, idx);
            continue;
        }

        if bytes.get(idx..idx + 2) == Some(b"/*") {
            idx = skip_block_comment(bytes, idx);
            continue;
        }

        if let Some(end_idx) = raw_string_end(bytes, idx) {
            tokens.push(Token {
                text: "<literal>".to_string(),
                kind: TokenKind::Literal,
                start: idx,
                end: end_idx,
            });
            idx = end_idx;
            continue;
        }

        if byte == b'"' {
            let end_idx = skip_string(bytes, idx);
            tokens.push(Token {
                text: "<literal>".to_string(),
                kind: TokenKind::Literal,
                start: idx,
                end: end_idx,
            });
            idx = end_idx;
            continue;
        }

        if byte == b'\'' && should_skip_char_literal(bytes, idx) {
            let end_idx = skip_char_literal(bytes, idx);
            tokens.push(Token {
                text: "<literal>".to_string(),
                kind: TokenKind::Literal,
                start: idx,
                end: end_idx,
            });
            idx = end_idx;
            continue;
        }

        if is_ident_start(bytes, idx) {
            let start = idx;
            let raw_identifier = bytes.get(idx..idx + 2) == Some(b"r#");
            if raw_identifier {
                idx += 2;
            }
            idx += 1;
            while idx < bytes.len() && is_ident_continue(bytes[idx]) {
                idx += 1;
            }
            let text = if raw_identifier {
                source[start + 2..idx].to_string()
            } else {
                source[start..idx].to_string()
            };
            tokens.push(Token {
                text,
                kind: TokenKind::Ident,
                start,
                end: idx,
            });
            continue;
        }

        let Some(ch) = source[idx..].chars().next() else {
            break;
        };
        let end = idx + ch.len_utf8();
        tokens.push(Token {
            text: ch.to_string(),
            kind: TokenKind::Punct,
            start: idx,
            end,
        });
        idx = end;
    }

    tokens
}

fn is_ident_start(bytes: &[u8], idx: usize) -> bool {
    let Some(byte) = bytes.get(idx).copied() else {
        return false;
    };
    if byte == b'r'
        && bytes.get(idx + 1) == Some(&b'#')
        && bytes
            .get(idx + 2)
            .is_some_and(|next| next.is_ascii_alphabetic() || *next == b'_')
    {
        return true;
    }
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn skip_line_comment(bytes: &[u8], mut idx: usize) -> usize {
    while idx < bytes.len() && bytes[idx] != b'\n' {
        idx += 1;
    }
    idx
}

fn skip_block_comment(bytes: &[u8], mut idx: usize) -> usize {
    idx += 2;
    let mut depth = 1usize;
    while idx + 1 < bytes.len() && depth > 0 {
        if bytes.get(idx..idx + 2) == Some(b"/*") {
            depth += 1;
            idx += 2;
        } else if bytes.get(idx..idx + 2) == Some(b"*/") {
            depth = depth.saturating_sub(1);
            idx += 2;
        } else {
            idx += 1;
        }
    }
    idx
}

fn raw_string_end(bytes: &[u8], idx: usize) -> Option<usize> {
    if bytes.get(idx).copied() != Some(b'r') {
        return None;
    }
    let mut cursor = idx + 1;
    while bytes.get(cursor).copied() == Some(b'#') {
        cursor += 1;
    }
    if bytes.get(cursor).copied() != Some(b'"') {
        return None;
    }
    let hash_count = cursor.saturating_sub(idx + 1);
    cursor += 1;

    while cursor < bytes.len() {
        if bytes[cursor] == b'"' {
            let mut hashes_match = true;
            for offset in 0..hash_count {
                if bytes.get(cursor + 1 + offset).copied() != Some(b'#') {
                    hashes_match = false;
                    break;
                }
            }
            if hashes_match {
                return Some(cursor + 1 + hash_count);
            }
        }
        cursor += 1;
    }
    Some(bytes.len())
}

fn skip_string(bytes: &[u8], mut idx: usize) -> usize {
    idx += 1;
    while idx < bytes.len() {
        match bytes[idx] {
            b'\\' => idx = (idx + 2).min(bytes.len()),
            b'"' => return idx + 1,
            _ => idx += 1,
        }
    }
    idx
}

fn should_skip_char_literal(bytes: &[u8], idx: usize) -> bool {
    let Some(next) = bytes.get(idx + 1).copied() else {
        return false;
    };
    !(next.is_ascii_alphabetic() || next == b'_')
}

fn skip_char_literal(bytes: &[u8], mut idx: usize) -> usize {
    idx += 1;
    while idx < bytes.len() {
        match bytes[idx] {
            b'\\' => idx = (idx + 2).min(bytes.len()),
            b'\'' => return idx + 1,
            b'\n' => return idx,
            _ => idx += 1,
        }
    }
    idx
}

fn brace_pairs(
    path: &str,
    tokens: &[Token],
) -> (HashMap<usize, usize>, Vec<SymbolGraphDegradation>) {
    let mut stack = Vec::new();
    let mut pairs = HashMap::new();
    let mut degraded = Vec::new();

    for (idx, token) in tokens.iter().enumerate() {
        match token.text.as_str() {
            "{" => stack.push(idx),
            "}" => {
                if let Some(open_idx) = stack.pop() {
                    pairs.insert(open_idx, idx);
                } else {
                    degraded.push(degradation(
                        SymbolGraphDegradationCode::SourceUnparsable,
                        Some(path.to_string()),
                        "closing brace has no matching opening brace".to_string(),
                    ));
                }
            }
            _ => {}
        }
    }

    if !stack.is_empty() {
        degraded.push(degradation(
            SymbolGraphDegradationCode::SourceUnparsable,
            Some(path.to_string()),
            "one or more opening braces have no matching closing brace".to_string(),
        ));
    }

    (pairs, degraded)
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (idx, byte) in source.as_bytes().iter().enumerate() {
        if *byte == b'\n' {
            starts.push(idx + 1);
        }
    }
    starts
}

fn byte_position(line_starts: &[usize], byte: usize) -> (u32, u32) {
    let line_idx = line_starts
        .partition_point(|start| *start <= byte)
        .saturating_sub(1);
    let line_start = line_starts.get(line_idx).copied().unwrap_or(0);
    (
        (line_idx + 1) as u32,
        byte.saturating_sub(line_start) as u32 + 1,
    )
}

fn canonical_name(namespace: &[String], local_name: &str) -> String {
    if namespace.is_empty() {
        local_name.to_string()
    } else {
        format!("{}::{local_name}", namespace.join("::"))
    }
}

fn rename_fingerprint(kind: SymbolKind, namespace: &[String], signature_shape: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, kind.as_str().as_bytes());
    hash_part(&mut hasher, namespace.join("::").as_bytes());
    hash_part(&mut hasher, signature_shape.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn symbol_id(
    path: &str,
    namespace: &[String],
    kind: SymbolKind,
    canonical_name: &str,
    rename_fingerprint: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, path.as_bytes());
    hash_part(&mut hasher, namespace.join("::").as_bytes());
    hash_part(&mut hasher, kind.as_str().as_bytes());
    hash_part(&mut hasher, canonical_name.as_bytes());
    hash_part(&mut hasher, rename_fingerprint.as_bytes());
    format!(
        "{}{}",
        SYMBOL_ID_PREFIX,
        &hasher.finalize().to_hex().to_string()[..24]
    )
}

fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn is_json_schema_constant(name: &str) -> bool {
    name.ends_with("_SCHEMA_V1")
        || name.ends_with("_SCHEMA_V2")
        || name.contains("_SCHEMA_CATALOG_V")
}

fn is_cli_command_handler(name: &str, namespace: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with("_command")
        || lower.starts_with("run_")
        || lower.starts_with("handle_")
        || namespace.iter().any(|part| part == "cli")
}

fn normalize_path_for_order(path: &Path) -> String {
    normalize_path_string(&path.to_string_lossy())
}

fn workspace_relative_path(workspace_root: &Path, path: &Path) -> String {
    path.strip_prefix(workspace_root)
        .map_or_else(|_| normalize_path_for_order(path), normalize_path_for_order)
}

fn normalize_path_string(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use serde::Serialize;

    const SAMPLE: &str = r#"
pub mod outer {
    pub const SEARCH_SCHEMA_V1: &str = "ee.search.v1";

    pub struct Engine {
        value: usize,
    }

    pub enum Mode {
        Fast,
        Slow,
    }

    pub trait Searcher {
        fn search(&self);
    }

    impl Engine {
        pub fn new() -> Self {
            Self { value: 0 }
        }

        fn helper(&self) {}
    }

    pub fn run_search_command() {}
}
"#;

    #[test]
    fn extracts_core_rust_symbol_shapes() {
        let snapshot = extract_rust_symbol_snapshot_from_sources(&[RustSourceInput::new(
            "src/sample.rs",
            SAMPLE,
        )]);
        let names: Vec<&str> = snapshot
            .symbols
            .iter()
            .map(|symbol| symbol.canonical_name.as_str())
            .collect();

        assert_eq!(snapshot.schema, SYMBOL_SNAPSHOT_SCHEMA_V1);
        assert!(snapshot.degraded.is_empty(), "{:?}", snapshot.degraded);
        assert!(names.contains(&"outer"));
        assert!(names.contains(&"outer::SEARCH_SCHEMA_V1"));
        assert!(names.contains(&"outer::Engine"));
        assert!(names.contains(&"outer::Mode"));
        assert!(names.contains(&"outer::Searcher"));
        assert!(names.contains(&"outer::Searcher::search"));
        assert!(names.contains(&"outer::impl Engine"));
        assert!(names.contains(&"outer::impl Engine::new"));
        assert!(names.contains(&"outer::impl Engine::helper"));
        assert!(names.contains(&"outer::run_search_command"));

        let cli_handler = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.canonical_name == "outer::run_search_command")
            .expect("CLI handler symbol");
        assert_eq!(cli_handler.kind, SymbolKind::CliCommandHandler);
        assert_eq!(cli_handler.visibility, SymbolVisibility::Public);
        assert!(cli_handler.id.starts_with(SYMBOL_ID_PREFIX));
    }

    #[test]
    fn treats_top_level_macros_as_opaque_symbols() {
        let source = r#"
macro_rules! route {
    () => {};
}

command_builder! {
    route => run_search_command
}
"#;
        let snapshot = extract_rust_symbol_snapshot_from_sources(&[RustSourceInput::new(
            "src/macros.rs",
            source,
        )]);
        let macro_symbols: Vec<&SymbolRecord> = snapshot
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == SymbolKind::MacroInvocation)
            .collect();

        assert_eq!(macro_symbols.len(), 2);
        assert_eq!(macro_symbols[0].canonical_name, "macro_rules! route");
        assert_eq!(macro_symbols[1].canonical_name, "command_builder!");
    }

    #[test]
    fn invalid_rust_degrades_without_storing_source_body() {
        let snapshot = extract_rust_symbol_snapshot_from_sources(&[RustSourceInput::new(
            "src/broken.rs",
            "pub mod broken { pub fn unfinished() {",
        )]);

        assert!(snapshot.degraded.iter().any(|item| {
            item.code == SymbolGraphDegradationCode::SourceUnparsable
                && item.path.as_deref() == Some("src/broken.rs")
        }));
        let json = serde_json::to_string(&snapshot).expect("symbol snapshot JSON");
        assert!(!json.contains("unfinished() {"));
    }

    #[test]
    fn stable_ids_are_unique_and_repeatable() {
        let first = extract_rust_symbol_snapshot_from_sources(&[RustSourceInput::new(
            "src/sample.rs",
            SAMPLE,
        )]);
        let second = extract_rust_symbol_snapshot_from_sources(&[RustSourceInput::new(
            "src/sample.rs",
            SAMPLE,
        )]);
        let first_ids: Vec<&str> = first
            .symbols
            .iter()
            .map(|symbol| symbol.id.as_str())
            .collect();
        let second_ids: Vec<&str> = second
            .symbols
            .iter()
            .map(|symbol| symbol.id.as_str())
            .collect();
        let unique: HashSet<&str> = first_ids.iter().copied().collect();

        assert_eq!(first.snapshot_hash, second.snapshot_hash);
        assert_eq!(first_ids, second_ids);
        assert_eq!(unique.len(), first_ids.len());
    }

    #[test]
    fn symbol_snapshot_json_golden_is_stable() {
        let snapshot = extract_rust_symbol_snapshot_from_sources(&[RustSourceInput::new(
            "src/sample.rs",
            SAMPLE,
        )]);
        let actual = serde_json::to_string_pretty(&redacted_snapshot(&snapshot))
            .expect("symbol snapshot JSON");

        assert_eq!(
            format!("{}\n", actual),
            include_str!("../../tests/fixtures/golden/symbol_graph/rust_snapshot.json.golden")
        );
    }

    #[test]
    fn links_memory_and_cass_evidence_to_nearest_symbols_without_source_body() {
        let snapshot = extract_rust_symbol_snapshot_from_sources(&[RustSourceInput::new(
            "src/sample.rs",
            SAMPLE,
        )]);
        let helper = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.canonical_name == "outer::impl Engine::helper")
            .expect("helper symbol");
        let runner = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.canonical_name == "outer::run_search_command")
            .expect("run_search_command symbol");

        let link_set = link_symbol_evidence(
            &snapshot,
            &[
                SymbolEvidenceInput::new(
                    SymbolEvidenceSourceKind::Memory,
                    "mem_01234567890123456789012345",
                    "file://src/sample.rs#L1",
                    "src/sample.rs",
                    helper.range.start_line,
                    helper.range.end_line,
                    0.93,
                ),
                SymbolEvidenceInput::new(
                    SymbolEvidenceSourceKind::CassEvidence,
                    "ev_cass_session_a_l2",
                    "cass-session://session-a#L2-L2",
                    "src/sample.rs",
                    runner.range.start_line,
                    runner.range.end_line,
                    0.81,
                ),
            ],
        );

        assert_eq!(link_set.schema, SYMBOL_EVIDENCE_LINKS_SCHEMA_V1);
        assert_eq!(link_set.snapshot_hash, snapshot.snapshot_hash);
        assert_eq!(link_set.links.len(), 2);
        assert!(link_set.degraded.is_empty(), "{:?}", link_set.degraded);

        let memory_link = link_set
            .links
            .iter()
            .find(|link| link.source_kind == SymbolEvidenceSourceKind::Memory)
            .expect("memory link");
        assert_eq!(memory_link.symbol_id.as_deref(), Some(helper.id.as_str()));
        assert_eq!(
            memory_link.reason,
            SymbolEvidenceReasonCode::ExactSymbolSpan
        );

        let cass_link = link_set
            .links
            .iter()
            .find(|link| link.source_kind == SymbolEvidenceSourceKind::CassEvidence)
            .expect("cass evidence link");
        assert_eq!(cass_link.symbol_id.as_deref(), Some(runner.id.as_str()));
        assert_eq!(cass_link.provenance_uri, "cass-session://session-a#L2-L2");

        let json = serde_json::to_string(&link_set).expect("symbol evidence link JSON");
        assert!(json.contains("ee.symbol_evidence_links.v1"));
        assert!(!json.contains("fn helper"));
        assert!(!json.contains("Self { value: 0 }"));
    }

    #[test]
    fn evidence_linker_reports_stale_renamed_deleted_and_ambiguous_spans() {
        let mut snapshot = extract_rust_symbol_snapshot_from_sources(&[RustSourceInput::new(
            "src/sample.rs",
            SAMPLE,
        )]);
        let engine = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.canonical_name == "outer::Engine")
            .expect("engine symbol")
            .clone();
        let helper = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.canonical_name == "outer::impl Engine::helper")
            .expect("helper symbol")
            .clone();

        let mut duplicate_helper = helper.clone();
        duplicate_helper.id = "sym_v1_aaaaaaaaaaaaaaaaaaaaaaaa".to_owned();
        duplicate_helper.canonical_name = "outer::impl Engine::helper_duplicate".to_owned();
        snapshot.symbols.push(duplicate_helper);

        let link_set = link_symbol_evidence(
            &snapshot,
            &[
                SymbolEvidenceInput::new(
                    SymbolEvidenceSourceKind::Memory,
                    "stale-span",
                    "file://src/sample.rs#L10-9",
                    "src/sample.rs",
                    10,
                    9,
                    1.0,
                ),
                SymbolEvidenceInput::new(
                    SymbolEvidenceSourceKind::Rule,
                    "renamed-symbol",
                    "file://src/sample.rs#L1",
                    "src/sample.rs",
                    engine.range.start_line,
                    engine.range.end_line,
                    0.7,
                )
                .with_expected_symbol(
                    "sym_v1_missingmissingmissing01",
                    engine.rename_fingerprint.as_str(),
                ),
                SymbolEvidenceInput::new(
                    SymbolEvidenceSourceKind::Decision,
                    "deleted-symbol",
                    "file://src/sample.rs#L1",
                    "src/sample.rs",
                    engine.range.start_line,
                    engine.range.end_line,
                    0.7,
                )
                .with_expected_symbol("sym_v1_missingmissingmissing02", "no-match"),
                SymbolEvidenceInput::new(
                    SymbolEvidenceSourceKind::CassEvidence,
                    "ambiguous-symbol",
                    "cass-session://session-b#L3",
                    "src/sample.rs",
                    helper.range.start_line,
                    helper.range.end_line,
                    0.9,
                ),
            ],
        );

        let by_id = |evidence_id: &str| {
            link_set
                .links
                .iter()
                .find(|link| link.evidence_id == evidence_id)
                .expect("evidence link")
        };
        assert_eq!(
            by_id("stale-span").resolution,
            SymbolEvidenceResolution::StaleSpan
        );
        assert_eq!(
            by_id("renamed-symbol").resolution,
            SymbolEvidenceResolution::RenamedSymbol
        );
        assert_eq!(
            by_id("deleted-symbol").resolution,
            SymbolEvidenceResolution::DeletedSymbol
        );
        assert_eq!(
            by_id("ambiguous-symbol").resolution,
            SymbolEvidenceResolution::Ambiguous
        );
        assert!(link_set.degraded.iter().any(|item| {
            item.code == SymbolEvidenceLinkDegradationCode::AmbiguousContainingSymbols
                && item.evidence_id == "ambiguous-symbol"
        }));
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct RedactedSnapshot<'a> {
        schema: &'a str,
        workspace_root: Option<&'a str>,
        generated_by: &'a str,
        files: Vec<RedactedFile<'a>>,
        symbols: Vec<RedactedSymbol<'a>>,
        degraded: &'a [SymbolGraphDegradation],
        snapshot_hash: &'static str,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct RedactedFile<'a> {
        path: &'a str,
        language: SymbolSourceLanguage,
        parser: SymbolParserKind,
        source_hash: &'static str,
        byte_len: &'static str,
        symbol_count: usize,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct RedactedSymbol<'a> {
        id: &'static str,
        kind: SymbolKind,
        canonical_name: &'a str,
        namespace: &'a [String],
        path: &'a str,
        range: RedactedRange,
        visibility: SymbolVisibility,
        declaration_hash: &'static str,
        rename_fingerprint: &'static str,
        parser: SymbolParserKind,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct RedactedRange {
        start_line: &'static str,
        start_column: &'static str,
        end_line: &'static str,
        end_column: &'static str,
    }

    fn redacted_snapshot(snapshot: &SymbolSnapshot) -> RedactedSnapshot<'_> {
        RedactedSnapshot {
            schema: snapshot.schema.as_str(),
            workspace_root: snapshot.workspace_root.as_deref(),
            generated_by: snapshot.generated_by.as_str(),
            files: snapshot
                .files
                .iter()
                .map(|file| RedactedFile {
                    path: file.path.as_str(),
                    language: file.language,
                    parser: file.parser,
                    source_hash: "<sourceHash>",
                    byte_len: "<byteLen>",
                    symbol_count: file.symbol_count,
                })
                .collect(),
            symbols: snapshot
                .symbols
                .iter()
                .map(|symbol| RedactedSymbol {
                    id: "<symbolId>",
                    kind: symbol.kind,
                    canonical_name: symbol.canonical_name.as_str(),
                    namespace: &symbol.namespace,
                    path: symbol.path.as_str(),
                    range: RedactedRange {
                        start_line: "<startLine>",
                        start_column: "<startColumn>",
                        end_line: "<endLine>",
                        end_column: "<endColumn>",
                    },
                    visibility: symbol.visibility,
                    declaration_hash: "<declarationHash>",
                    rename_fingerprint: "<renameFingerprint>",
                    parser: symbol.parser,
                })
                .collect(),
            degraded: &snapshot.degraded,
            snapshot_hash: "<snapshotHash>",
        }
    }
}
