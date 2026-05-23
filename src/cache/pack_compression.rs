//! Deterministic zstd dictionary training for context-pack compression.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

pub const PACK_COMPRESSION_MANIFEST_SCHEMA_V1: &str = "ee.pack.compression_manifest.v1";
pub const PACK_COMPRESSION_TRAINING_ALGORITHM_V1: &str = "zstd_train_from_pack_corpus_v1";
pub const DEFAULT_PACK_COMPRESSION_DICTIONARY_BYTES: usize = 64 * 1024;
pub const DEFAULT_PACK_COMPRESSION_SAMPLE_COUNT: usize = 256;
pub const DEFAULT_PACK_COMPRESSION_SAMPLE_BYTES: usize = 8 * 1024 * 1024;
const PACK_COMPRESSION_DERIVED_DIR: &str = "pack-compression";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackCompressionSampleSourceKind {
    PackRecord,
    L2CacheEntry,
    FixtureCorpus,
}

impl PackCompressionSampleSourceKind {
    #[must_use]
    pub const fn corpus_window_kind(self) -> &'static str {
        match self {
            Self::PackRecord => "pack_records",
            Self::L2CacheEntry => "l2_cache_entries",
            Self::FixtureCorpus => "fixture_corpus",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackCompressionSample {
    pub source_kind: PackCompressionSampleSourceKind,
    pub source_id: String,
    pub generation: Option<u64>,
    pub payload: Vec<u8>,
    pub redaction_level: Option<String>,
}

impl PackCompressionSample {
    #[must_use]
    pub fn new(
        source_kind: PackCompressionSampleSourceKind,
        source_id: impl Into<String>,
        generation: Option<u64>,
        payload: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            source_kind,
            source_id: source_id.into(),
            generation,
            payload: payload.into(),
            redaction_level: None,
        }
    }

    #[must_use]
    pub fn with_redaction_level(mut self, redaction_level: impl Into<String>) -> Self {
        self.redaction_level = Some(redaction_level.into());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackCompressionTrainingOptions {
    pub workspace_id: Option<String>,
    pub max_dictionary_bytes: usize,
    pub max_sample_count: usize,
    pub max_sample_bytes: usize,
    pub redaction_level: Option<String>,
}

impl Default for PackCompressionTrainingOptions {
    fn default() -> Self {
        Self {
            workspace_id: None,
            max_dictionary_bytes: DEFAULT_PACK_COMPRESSION_DICTIONARY_BYTES,
            max_sample_count: DEFAULT_PACK_COMPRESSION_SAMPLE_COUNT,
            max_sample_bytes: DEFAULT_PACK_COMPRESSION_SAMPLE_BYTES,
            redaction_level: Some(
                "ids_hashes_counts_paths_no_pack_content_no_query_text".to_owned(),
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackCompressionSampleIdentity {
    pub source_kind: PackCompressionSampleSourceKind,
    pub source_id_hash: String,
    pub generation: Option<u64>,
    pub payload_hash: String,
    pub byte_len: usize,
    pub redaction_level: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackCompressionCorpusWindow {
    pub source_kind: String,
    pub workspace_id: Option<String>,
    pub from_generation: Option<u64>,
    pub to_generation: Option<u64>,
    pub sample_count: usize,
    pub sample_hash: String,
    pub redaction_level: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackCompressionCorpusPlan {
    pub corpus_window: PackCompressionCorpusWindow,
    pub sample_identities: Vec<PackCompressionSampleIdentity>,
    pub payload_bytes: usize,
    pub skipped_sample_count: usize,
    pub skipped_sample_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackCompressionDictionaryTrainingReport {
    pub dictionary_id: Option<String>,
    pub dictionary_byte_hash: Option<String>,
    pub dictionary_source_hash: String,
    pub dictionary_bytes: Vec<u8>,
    pub dictionary_byte_len: usize,
    pub corpus_window: PackCompressionCorpusWindow,
    pub sample_identities: Vec<PackCompressionSampleIdentity>,
    pub payload_bytes: usize,
    pub skipped_sample_count: usize,
    pub skipped_sample_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackCompressionDictionaryTrainingOutcome {
    Trained(PackCompressionDictionaryTrainingReport),
    NoEligibleSamples(PackCompressionDictionaryTrainingReport),
}

impl PackCompressionDictionaryTrainingOutcome {
    #[must_use]
    pub fn report(&self) -> &PackCompressionDictionaryTrainingReport {
        match self {
            Self::Trained(report) | Self::NoEligibleSamples(report) => report,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackCompressionDictionaryFreshness {
    Missing,
    Fresh {
        dictionary_source_hash: String,
        sample_hash: String,
    },
    Stale {
        expected_dictionary_source_hash: String,
        actual_dictionary_source_hash: String,
        expected_sample_hash: Option<String>,
        actual_sample_hash: String,
    },
}

#[derive(Debug)]
pub enum PackCompressionTrainingError {
    InvalidOptions { message: &'static str },
    DictionaryTraining { source: std::io::Error },
    Json { source: serde_json::Error },
}

impl fmt::Display for PackCompressionTrainingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOptions { message } => write!(formatter, "{message}"),
            Self::DictionaryTraining { source } => {
                write!(formatter, "failed to train zstd pack dictionary: {source}")
            }
            Self::Json { source } => {
                write!(
                    formatter,
                    "failed to serialize pack compression corpus identity: {source}"
                )
            }
        }
    }
}

impl std::error::Error for PackCompressionTrainingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DictionaryTraining { source } => Some(source),
            Self::Json { source } => Some(source),
            Self::InvalidOptions { .. } => None,
        }
    }
}

#[must_use]
pub fn pack_compression_derived_root(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(".ee")
        .join("derived")
        .join(PACK_COMPRESSION_DERIVED_DIR)
}

#[must_use]
pub fn pack_compression_manifest_path(workspace_root: &Path, manifest_id: &str) -> PathBuf {
    pack_compression_derived_root(workspace_root)
        .join("manifests")
        .join(format!("{manifest_id}.json"))
}

#[must_use]
pub fn pack_compression_dictionary_path(workspace_root: &Path, dictionary_id: &str) -> PathBuf {
    pack_compression_derived_root(workspace_root)
        .join("dictionaries")
        .join(format!("{dictionary_id}.dict"))
}

pub fn plan_pack_compression_training_corpus(
    samples: &[PackCompressionSample],
    options: &PackCompressionTrainingOptions,
) -> Result<PackCompressionCorpusPlan, PackCompressionTrainingError> {
    Ok(build_training_corpus(samples, options)?.into_plan())
}

pub fn train_pack_compression_dictionary(
    samples: &[PackCompressionSample],
    options: &PackCompressionTrainingOptions,
) -> Result<PackCompressionDictionaryTrainingOutcome, PackCompressionTrainingError> {
    let corpus = build_training_corpus(samples, options)?;
    let dictionary_source_hash = dictionary_source_hash(&corpus, options)?;
    if corpus.payloads.is_empty() {
        return Ok(PackCompressionDictionaryTrainingOutcome::NoEligibleSamples(
            corpus.into_report(None, None, dictionary_source_hash, Vec::new()),
        ));
    }

    let mut continuous_payload = Vec::with_capacity(corpus.payload_bytes);
    let mut sample_sizes = Vec::with_capacity(corpus.payloads.len());
    for payload in &corpus.payloads {
        sample_sizes.push(payload.len());
        continuous_payload.extend_from_slice(payload);
    }

    let dictionary_bytes = zstd::dict::from_continuous(
        &continuous_payload,
        &sample_sizes,
        options.max_dictionary_bytes,
    )
    .map_err(|source| PackCompressionTrainingError::DictionaryTraining { source })?;
    let dictionary_byte_hash = blake3_hash(&dictionary_bytes);
    let dictionary_id = format!("zstd_dict_{}", blake3_hex(&dictionary_bytes));
    let report = corpus.into_report(
        Some(dictionary_id),
        Some(dictionary_byte_hash),
        dictionary_source_hash,
        dictionary_bytes,
    );

    Ok(PackCompressionDictionaryTrainingOutcome::Trained(report))
}

pub fn assess_pack_compression_dictionary_freshness(
    recorded_dictionary_source_hash: Option<&str>,
    recorded_sample_hash: Option<&str>,
    samples: &[PackCompressionSample],
    options: &PackCompressionTrainingOptions,
) -> Result<PackCompressionDictionaryFreshness, PackCompressionTrainingError> {
    let Some(expected_dictionary_source_hash) = recorded_dictionary_source_hash else {
        return Ok(PackCompressionDictionaryFreshness::Missing);
    };
    let corpus = build_training_corpus(samples, options)?;
    let actual_dictionary_source_hash = dictionary_source_hash(&corpus, options)?;
    let actual_sample_hash = corpus.corpus_window.sample_hash;
    if expected_dictionary_source_hash == actual_dictionary_source_hash
        && recorded_sample_hash.is_none_or(|expected| expected == actual_sample_hash)
    {
        return Ok(PackCompressionDictionaryFreshness::Fresh {
            dictionary_source_hash: actual_dictionary_source_hash,
            sample_hash: actual_sample_hash,
        });
    }

    Ok(PackCompressionDictionaryFreshness::Stale {
        expected_dictionary_source_hash: expected_dictionary_source_hash.to_owned(),
        actual_dictionary_source_hash,
        expected_sample_hash: recorded_sample_hash.map(str::to_owned),
        actual_sample_hash,
    })
}

#[derive(Debug)]
struct PlannedPackCompressionCorpus {
    corpus_window: PackCompressionCorpusWindow,
    sample_identities: Vec<PackCompressionSampleIdentity>,
    payloads: Vec<Vec<u8>>,
    payload_bytes: usize,
    skipped_sample_count: usize,
    skipped_sample_bytes: usize,
}

impl PlannedPackCompressionCorpus {
    fn into_plan(self) -> PackCompressionCorpusPlan {
        PackCompressionCorpusPlan {
            corpus_window: self.corpus_window,
            sample_identities: self.sample_identities,
            payload_bytes: self.payload_bytes,
            skipped_sample_count: self.skipped_sample_count,
            skipped_sample_bytes: self.skipped_sample_bytes,
        }
    }

    fn into_report(
        self,
        dictionary_id: Option<String>,
        dictionary_byte_hash: Option<String>,
        dictionary_source_hash: String,
        dictionary_bytes: Vec<u8>,
    ) -> PackCompressionDictionaryTrainingReport {
        let dictionary_byte_len = dictionary_bytes.len();
        PackCompressionDictionaryTrainingReport {
            dictionary_id,
            dictionary_byte_hash,
            dictionary_source_hash,
            dictionary_bytes,
            dictionary_byte_len,
            corpus_window: self.corpus_window,
            sample_identities: self.sample_identities,
            payload_bytes: self.payload_bytes,
            skipped_sample_count: self.skipped_sample_count,
            skipped_sample_bytes: self.skipped_sample_bytes,
        }
    }
}

#[derive(Debug)]
struct CandidateSample<'a> {
    identity: PackCompressionSampleIdentity,
    payload: &'a [u8],
}

fn build_training_corpus(
    samples: &[PackCompressionSample],
    options: &PackCompressionTrainingOptions,
) -> Result<PlannedPackCompressionCorpus, PackCompressionTrainingError> {
    if options.max_dictionary_bytes == 0 {
        return Err(PackCompressionTrainingError::InvalidOptions {
            message: "pack compression dictionary byte cap must be greater than zero",
        });
    }

    let mut candidates = samples
        .iter()
        .filter(|sample| !sample.payload.is_empty())
        .map(|sample| CandidateSample {
            identity: sample_identity(sample),
            payload: sample.payload.as_slice(),
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.identity
            .payload_hash
            .cmp(&right.identity.payload_hash)
            .then_with(|| left.identity.source_kind.cmp(&right.identity.source_kind))
            .then_with(|| {
                left.identity
                    .source_id_hash
                    .cmp(&right.identity.source_id_hash)
            })
            .then_with(|| left.identity.generation.cmp(&right.identity.generation))
    });

    let mut payload_bytes = 0_usize;
    let mut skipped_sample_count = 0_usize;
    let mut skipped_sample_bytes = 0_usize;
    let mut sample_identities = Vec::new();
    let mut payloads = Vec::new();
    for candidate in candidates {
        let would_exceed_count = sample_identities.len() >= options.max_sample_count;
        let would_exceed_bytes = payload_bytes
            .checked_add(candidate.payload.len())
            .is_none_or(|next| next > options.max_sample_bytes);
        if would_exceed_count || would_exceed_bytes {
            skipped_sample_count = skipped_sample_count.saturating_add(1);
            skipped_sample_bytes = skipped_sample_bytes.saturating_add(candidate.payload.len());
            continue;
        }

        payload_bytes = payload_bytes.saturating_add(candidate.payload.len());
        sample_identities.push(candidate.identity);
        payloads.push(candidate.payload.to_vec());
    }

    let sample_hash = blake3_json_hash(&sample_identities)?;
    let source_kind = corpus_source_kind(&sample_identities);
    let from_generation = sample_identities
        .iter()
        .filter_map(|identity| identity.generation)
        .min();
    let to_generation = sample_identities
        .iter()
        .filter_map(|identity| identity.generation)
        .max();
    let mut notes = Vec::new();
    if sample_identities.is_empty() {
        notes.push("no_eligible_samples".to_owned());
    }
    if skipped_sample_count > 0 {
        notes.push("sample_budget_truncated".to_owned());
    }

    let corpus_window = PackCompressionCorpusWindow {
        source_kind,
        workspace_id: options.workspace_id.clone(),
        from_generation,
        to_generation,
        sample_count: sample_identities.len(),
        sample_hash,
        redaction_level: options.redaction_level.clone(),
        notes,
    };

    Ok(PlannedPackCompressionCorpus {
        corpus_window,
        sample_identities,
        payloads,
        payload_bytes,
        skipped_sample_count,
        skipped_sample_bytes,
    })
}

fn sample_identity(sample: &PackCompressionSample) -> PackCompressionSampleIdentity {
    PackCompressionSampleIdentity {
        source_kind: sample.source_kind,
        source_id_hash: blake3_hash(sample.source_id.as_bytes()),
        generation: sample.generation,
        payload_hash: blake3_hash(&sample.payload),
        byte_len: sample.payload.len(),
        redaction_level: sample.redaction_level.clone(),
    }
}

fn corpus_source_kind(identities: &[PackCompressionSampleIdentity]) -> String {
    let source_kinds = identities
        .iter()
        .map(|identity| identity.source_kind.corpus_window_kind())
        .collect::<BTreeSet<_>>();
    if source_kinds.len() == 1 {
        source_kinds
            .into_iter()
            .next()
            .unwrap_or("mixed")
            .to_owned()
    } else {
        "mixed".to_owned()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DictionarySourceIdentity<'a> {
    training_algorithm: &'static str,
    max_dictionary_bytes: usize,
    max_sample_count: usize,
    max_sample_bytes: usize,
    corpus_window: &'a PackCompressionCorpusWindow,
    sample_identities: &'a [PackCompressionSampleIdentity],
}

fn dictionary_source_hash(
    corpus: &PlannedPackCompressionCorpus,
    options: &PackCompressionTrainingOptions,
) -> Result<String, PackCompressionTrainingError> {
    blake3_json_hash(&DictionarySourceIdentity {
        training_algorithm: PACK_COMPRESSION_TRAINING_ALGORITHM_V1,
        max_dictionary_bytes: options.max_dictionary_bytes,
        max_sample_count: options.max_sample_count,
        max_sample_bytes: options.max_sample_bytes,
        corpus_window: &corpus.corpus_window,
        sample_identities: &corpus.sample_identities,
    })
}

fn blake3_json_hash(value: &impl Serialize) -> Result<String, PackCompressionTrainingError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|source| PackCompressionTrainingError::Json { source })?;
    Ok(blake3_hash(&bytes))
}

fn blake3_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3_hex(bytes))
}

fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn sample(
        source_kind: PackCompressionSampleSourceKind,
        source_id: &str,
        generation: u64,
        payload: &str,
    ) -> PackCompressionSample {
        PackCompressionSample::new(
            source_kind,
            source_id,
            Some(generation),
            payload.as_bytes().to_vec(),
        )
        .with_redaction_level("minimal")
    }

    #[test]
    fn corpus_plan_sorts_by_payload_hash_and_applies_stable_budget() -> TestResult {
        let samples = vec![
            sample(
                PackCompressionSampleSourceKind::PackRecord,
                "pack-z",
                30,
                "zzzzzzzzzz",
            ),
            sample(
                PackCompressionSampleSourceKind::L2CacheEntry,
                "cache-a",
                10,
                "aaaaaaaaaa",
            ),
            sample(
                PackCompressionSampleSourceKind::PackRecord,
                "pack-m",
                20,
                "mmmmmmmmmm",
            ),
        ];
        let options = PackCompressionTrainingOptions {
            max_sample_count: 2,
            max_sample_bytes: 20,
            ..PackCompressionTrainingOptions::default()
        };

        let first = plan_pack_compression_training_corpus(&samples, &options)
            .map_err(|error| error.to_string())?;
        let mut reversed = samples;
        reversed.reverse();
        let second = plan_pack_compression_training_corpus(&reversed, &options)
            .map_err(|error| error.to_string())?;

        assert_eq!(
            first.sample_identities, second.sample_identities,
            "sample order must not depend on input order"
        );
        assert_eq!(first.corpus_window.sample_count, 2);
        assert_eq!(first.payload_bytes, 20);
        assert_eq!(first.skipped_sample_count, 1);
        assert_eq!(
            first.corpus_window.source_kind, "mixed",
            "mixed L2/pack samples should use the mixed source-kind envelope"
        );
        assert!(
            first
                .corpus_window
                .notes
                .contains(&"sample_budget_truncated".to_owned()),
            "budget truncation should be explicit in manifest metadata"
        );
        Ok(())
    }

    #[test]
    fn no_eligible_samples_returns_redaction_safe_empty_report() -> TestResult {
        let options = PackCompressionTrainingOptions::default();
        let outcome = train_pack_compression_dictionary(
            &[PackCompressionSample::new(
                PackCompressionSampleSourceKind::FixtureCorpus,
                "empty-fixture",
                None,
                Vec::new(),
            )],
            &options,
        )
        .map_err(|error| error.to_string())?;

        let report = match outcome {
            PackCompressionDictionaryTrainingOutcome::NoEligibleSamples(report) => report,
            PackCompressionDictionaryTrainingOutcome::Trained(_) => {
                return Err("empty corpus must not train a dictionary".to_owned());
            }
        };
        assert!(report.dictionary_id.is_none());
        assert!(report.dictionary_byte_hash.is_none());
        assert!(report.dictionary_bytes.is_empty());
        assert_eq!(report.corpus_window.sample_count, 0);
        assert!(
            report
                .corpus_window
                .notes
                .contains(&"no_eligible_samples".to_owned())
        );
        Ok(())
    }

    #[test]
    fn dictionary_training_is_stable_for_same_payload_set() -> TestResult {
        let mut samples = Vec::new();
        for index in 0..96 {
            let payload = format!(
                "{{\"schema\":\"ee.pack.v2\",\"pack\":\"release\",\"index\":{index},\"items\":[\"mem_alpha\",\"mem_beta\"],\"provenance\":\"deterministic-pack-corpus\"}}"
            );
            samples.push(sample(
                PackCompressionSampleSourceKind::PackRecord,
                &format!("pack-{index:03}"),
                index,
                &payload,
            ));
        }
        let mut reversed = samples.clone();
        reversed.reverse();
        let options = PackCompressionTrainingOptions {
            max_dictionary_bytes: 8192,
            max_sample_count: 128,
            max_sample_bytes: 256 * 1024,
            ..PackCompressionTrainingOptions::default()
        };

        let first = train_pack_compression_dictionary(&samples, &options)
            .map_err(|error| error.to_string())?;
        let second = train_pack_compression_dictionary(&reversed, &options)
            .map_err(|error| error.to_string())?;
        let first = match first {
            PackCompressionDictionaryTrainingOutcome::Trained(report) => report,
            PackCompressionDictionaryTrainingOutcome::NoEligibleSamples(_) => {
                return Err("fixture corpus should train a dictionary".to_owned());
            }
        };
        let second = match second {
            PackCompressionDictionaryTrainingOutcome::Trained(report) => report,
            PackCompressionDictionaryTrainingOutcome::NoEligibleSamples(_) => {
                return Err("fixture corpus should train a dictionary".to_owned());
            }
        };

        assert_eq!(first.dictionary_id, second.dictionary_id);
        assert_eq!(first.dictionary_byte_hash, second.dictionary_byte_hash);
        assert_eq!(first.dictionary_source_hash, second.dictionary_source_hash);
        assert_eq!(first.dictionary_bytes, second.dictionary_bytes);
        assert_eq!(
            first.corpus_window.sample_hash,
            second.corpus_window.sample_hash
        );
        assert_eq!(first.corpus_window.sample_count, 96);
        Ok(())
    }

    #[test]
    fn dictionary_freshness_detects_missing_fresh_and_stale_corpus() -> TestResult {
        let samples = vec![
            sample(
                PackCompressionSampleSourceKind::PackRecord,
                "pack-a",
                1,
                "{\"schema\":\"ee.pack.v2\",\"items\":[\"mem_a\"]}",
            ),
            sample(
                PackCompressionSampleSourceKind::PackRecord,
                "pack-b",
                2,
                "{\"schema\":\"ee.pack.v2\",\"items\":[\"mem_b\"]}",
            ),
        ];
        let mut changed_samples = samples.clone();
        changed_samples.push(sample(
            PackCompressionSampleSourceKind::PackRecord,
            "pack-c",
            3,
            "{\"schema\":\"ee.pack.v2\",\"items\":[\"mem_c\"]}",
        ));
        let options = PackCompressionTrainingOptions {
            max_dictionary_bytes: 4096,
            max_sample_count: 16,
            max_sample_bytes: 32 * 1024,
            ..PackCompressionTrainingOptions::default()
        };
        let report = train_pack_compression_dictionary(&samples, &options)
            .map_err(|error| error.to_string())?
            .report()
            .clone();

        assert_eq!(
            assess_pack_compression_dictionary_freshness(None, None, &samples, &options)
                .map_err(|error| error.to_string())?,
            PackCompressionDictionaryFreshness::Missing
        );
        assert_eq!(
            assess_pack_compression_dictionary_freshness(
                Some(&report.dictionary_source_hash),
                Some(&report.corpus_window.sample_hash),
                &samples,
                &options
            )
            .map_err(|error| error.to_string())?,
            PackCompressionDictionaryFreshness::Fresh {
                dictionary_source_hash: report.dictionary_source_hash.clone(),
                sample_hash: report.corpus_window.sample_hash.clone(),
            }
        );
        assert!(
            matches!(
                assess_pack_compression_dictionary_freshness(
                    Some(&report.dictionary_source_hash),
                    Some(&report.corpus_window.sample_hash),
                    &changed_samples,
                    &options
                )
                .map_err(|error| error.to_string())?,
                PackCompressionDictionaryFreshness::Stale { .. }
            ),
            "changed corpus window should mark the dictionary metadata stale"
        );
        Ok(())
    }

    #[test]
    fn sidecar_paths_are_workspace_scoped_and_content_addressed() {
        let workspace = Path::new("/workspace/project");
        let manifest_id = "packcm_1111111111111111111111111111111111111111111111111111111111111111";
        let dictionary_id =
            "zstd_dict_2222222222222222222222222222222222222222222222222222222222222222";

        assert_eq!(
            pack_compression_manifest_path(workspace, manifest_id),
            PathBuf::from("/workspace/project/.ee/derived/pack-compression/manifests")
                .join(format!("{manifest_id}.json"))
        );
        assert_eq!(
            pack_compression_dictionary_path(workspace, dictionary_id),
            PathBuf::from("/workspace/project/.ee/derived/pack-compression/dictionaries")
                .join(format!("{dictionary_id}.dict"))
        );
    }
}
