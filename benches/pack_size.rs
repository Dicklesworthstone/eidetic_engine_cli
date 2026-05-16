#![allow(clippy::expect_used)]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, black_box};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use ee::core::context::{ContextPackOptions, run_context_pack};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::DbConnection;
use ee::models::{MemoryScope, RedactionLevel};
use ee::output::{render_context_response_json, render_context_response_markdown};
use ee::search::SpeedMode;

const CORPUS_JSONL: &str = include_str!("../tests/fixtures/corpus/corpus_2026_05_10.jsonl");
const CORPUS_FIXTURE_PATH: &str = "tests/fixtures/corpus/corpus_2026_05_10.jsonl";
const PACK_SIZE_GROUP_NAME: &str = "pack_size";
const PACK_QUERY: &str = "prepare release v0.2.0";
const TOKEN_BUDGETS: &[u32] = &[500, 1000, 2000, 4000];
const SUMMARY_RELATIVE_PATH: &str = "criterion/pack_size/summary.json";

#[derive(Clone, Debug, Deserialize)]
struct CorpusRecord {
    content: String,
    level: String,
    kind: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "default_confidence")]
    confidence: f32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SeedReport {
    fixture: &'static str,
    total_records: usize,
    seeded_records: usize,
    rejected_records: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct PackSizeSample {
    max_tokens: u32,
    pack_json_bytes: usize,
    pack_text_bytes: usize,
    tokens_used_per_pack: u32,
    bytes_per_token: f64,
    item_count: usize,
    degraded_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct PackSizeMeasurement {
    value: f64,
    n: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct PackSizeMeasurements {
    pack_json_bytes_for_500_tokens: PackSizeMeasurement,
    pack_text_bytes_for_500_tokens: PackSizeMeasurement,
    tokens_used_per_pack_for_500_tokens: PackSizeMeasurement,
    bytes_per_token_for_500_tokens: PackSizeMeasurement,
    pack_json_bytes_for_1000_tokens: PackSizeMeasurement,
    pack_text_bytes_for_1000_tokens: PackSizeMeasurement,
    tokens_used_per_pack_for_1000_tokens: PackSizeMeasurement,
    bytes_per_token_for_1000_tokens: PackSizeMeasurement,
    pack_json_bytes_for_2000_tokens: PackSizeMeasurement,
    pack_text_bytes_for_2000_tokens: PackSizeMeasurement,
    tokens_used_per_pack_for_2000_tokens: PackSizeMeasurement,
    bytes_per_token_for_2000_tokens: PackSizeMeasurement,
    pack_json_bytes_for_4000_tokens: PackSizeMeasurement,
    pack_text_bytes_for_4000_tokens: PackSizeMeasurement,
    tokens_used_per_pack_for_4000_tokens: PackSizeMeasurement,
    bytes_per_token_for_4000_tokens: PackSizeMeasurement,
    bytes_per_token: PackSizeMeasurement,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct PackSizeSummary {
    schema: &'static str,
    corpus: SeedReport,
    query: &'static str,
    measurements: PackSizeMeasurements,
    samples: Vec<PackSizeSample>,
}

struct PackSizeFixture {
    _temp_dir: TempDir,
    workspace_path: PathBuf,
    db_path: PathBuf,
    seed_report: SeedReport,
}

fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let measure_only = args
        .iter()
        .any(|arg| arg == "--measure-only" || arg == "--quick");
    let print_json = args.iter().any(|arg| arg == "--summary-json");

    if measure_only {
        return match measure_and_write_summary(print_json) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        };
    }

    if let Err(error) = measure_and_write_summary(false) {
        eprintln!("warning: pack-size summary was not written before Criterion run: {error}");
    }

    run_criterion_mode();
    ExitCode::SUCCESS
}

fn run_criterion_mode() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_pack_size(&mut criterion);
    criterion.final_summary();
}

fn bench_pack_size(criterion: &mut Criterion) {
    let fixture = PackSizeFixture::prepare().expect("prepare pack-size fixture");
    let mut group = criterion.benchmark_group(PACK_SIZE_GROUP_NAME);
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(250));
    group.measurement_time(Duration::from_millis(500));

    for &max_tokens in TOKEN_BUDGETS {
        let label = format!("{max_tokens}_tokens");

        group.bench_with_input(
            BenchmarkId::new("pack_json_bytes", &label),
            &max_tokens,
            |bench, &tokens| {
                bench.iter(|| {
                    let sample = fixture
                        .measure_once(tokens)
                        .expect("measure pack JSON bytes");
                    black_box(sample.pack_json_bytes);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("pack_text_bytes", &label),
            &max_tokens,
            |bench, &tokens| {
                bench.iter(|| {
                    let sample = fixture
                        .measure_once(tokens)
                        .expect("measure pack text bytes");
                    black_box(sample.pack_text_bytes);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("tokens_used_per_pack", &label),
            &max_tokens,
            |bench, &tokens| {
                bench.iter(|| {
                    let sample = fixture.measure_once(tokens).expect("measure used tokens");
                    black_box(sample.tokens_used_per_pack);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("bytes_per_token", &label),
            &max_tokens,
            |bench, &tokens| {
                bench.iter(|| {
                    let sample = fixture
                        .measure_once(tokens)
                        .expect("measure bytes per token");
                    black_box(sample.bytes_per_token);
                });
            },
        );
    }

    group.finish();
}

fn measure_and_write_summary(print_json: bool) -> Result<(), String> {
    let fixture = PackSizeFixture::prepare()?;
    let summary = fixture.summary()?;
    let summary_json = serde_json::to_string_pretty(&summary)
        .map_err(|error| format!("serialize pack-size summary: {error}"))?;
    let path = summary_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create summary dir {}: {error}", parent.display()))?;
    }
    fs::write(&path, &summary_json)
        .map_err(|error| format!("write pack-size summary {}: {error}", path.display()))?;

    if print_json {
        println!("{summary_json}");
    } else {
        eprintln!("[pack_size] summary: {}", path.display());
    }

    Ok(())
}

impl PackSizeFixture {
    fn prepare() -> Result<Self, String> {
        let temp_dir = TempDir::new().map_err(|error| format!("create temp dir: {error}"))?;
        let workspace_path = temp_dir.path().to_path_buf();
        let db_path = workspace_path.join(".ee").join("ee.db");
        let db_parent = db_path
            .parent()
            .ok_or_else(|| format!("database path has no parent: {}", db_path.display()))?;
        fs::create_dir_all(db_parent)
            .map_err(|error| format!("create db dir {}: {error}", db_parent.display()))?;

        let connection = DbConnection::open_file(&db_path)
            .map_err(|error| format!("open db {}: {error}", db_path.display()))?;
        connection
            .migrate()
            .map_err(|error| format!("migrate db {}: {error}", db_path.display()))?;

        let records = parse_corpus_records()?;
        let mut seeded_records = 0_usize;
        let mut rejected_records = 0_usize;

        for record in &records {
            let tags = (!record.tags.is_empty()).then(|| record.tags.join(","));
            let options = RememberMemoryOptions {
                workspace_path: &workspace_path,
                database_path: Some(&db_path),
                content: &record.content,
                workflow_id: None,
                level: &record.level,
                kind: &record.kind,
                tags: tags.as_deref(),
                confidence: record.confidence,
                source: None,
                valid_from: None,
                valid_to: None,
                dry_run: false,
                auto_link: true,
                propose_candidates: false,
                allow_secret_mention: false,
            };

            match remember_memory(&options) {
                Ok(_) => seeded_records = seeded_records.saturating_add(1),
                Err(_) => rejected_records = rejected_records.saturating_add(1),
            }
        }

        if seeded_records == 0 {
            return Err("pack-size fixture seeded zero memories".to_string());
        }

        Ok(Self {
            _temp_dir: temp_dir,
            workspace_path,
            db_path,
            seed_report: SeedReport {
                fixture: CORPUS_FIXTURE_PATH,
                total_records: records.len(),
                seeded_records,
                rejected_records,
            },
        })
    }

    fn summary(&self) -> Result<PackSizeSummary, String> {
        let mut samples = Vec::with_capacity(TOKEN_BUDGETS.len());
        for &max_tokens in TOKEN_BUDGETS {
            samples.push(self.measure_once(max_tokens)?);
        }

        Ok(PackSizeSummary {
            schema: "ee.bench.pack_size.summary.v1",
            corpus: self.seed_report.clone(),
            query: PACK_QUERY,
            measurements: PackSizeMeasurements::from_samples(&samples)?,
            samples,
        })
    }

    fn measure_once(&self, max_tokens: u32) -> Result<PackSizeSample, String> {
        let options = ContextPackOptions {
            workspace_path: self.workspace_path.clone(),
            database_path: Some(self.db_path.clone()),
            index_dir: None,
            query: PACK_QUERY.to_string(),
            speed: SpeedMode::Default,
            filters: Default::default(),
            profile: None,
            max_tokens: Some(max_tokens),
            candidate_pool: Some(64),
            max_results: None,
            include_tombstoned: false,
            as_of: None,
            include_expired: false,
            include_future: false,
            include_stale: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            redaction_level: RedactionLevel::Minimal,
            ppr_weight: None,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            output_options: Default::default(),
        };
        let response =
            run_context_pack(&options).map_err(|error| format!("run context pack: {error}"))?;
        let json = render_context_response_json(&response);
        let markdown = render_context_response_markdown(&response);
        let tokens_used = response.data.pack.used_tokens;
        let denominator = f64::from(tokens_used.max(1));

        Ok(PackSizeSample {
            max_tokens,
            pack_json_bytes: json.len(),
            pack_text_bytes: markdown.len(),
            tokens_used_per_pack: tokens_used,
            bytes_per_token: json.len() as f64 / denominator,
            item_count: response.data.pack.items.len(),
            degraded_count: response.data.degraded.len(),
        })
    }
}

impl PackSizeMeasurements {
    fn from_samples(samples: &[PackSizeSample]) -> Result<Self, String> {
        let by_budget = |max_tokens: u32| -> Result<&PackSizeSample, String> {
            samples
                .iter()
                .find(|sample| sample.max_tokens == max_tokens)
                .ok_or_else(|| format!("missing sample for {max_tokens} tokens"))
        };

        let sample_500 = by_budget(500)?;
        let sample_1000 = by_budget(1000)?;
        let sample_2000 = by_budget(2000)?;
        let sample_4000 = by_budget(4000)?;

        Ok(Self {
            pack_json_bytes_for_500_tokens: measurement_usize(sample_500.pack_json_bytes),
            pack_text_bytes_for_500_tokens: measurement_usize(sample_500.pack_text_bytes),
            tokens_used_per_pack_for_500_tokens: measurement_u32(sample_500.tokens_used_per_pack),
            bytes_per_token_for_500_tokens: measurement_f64(sample_500.bytes_per_token),
            pack_json_bytes_for_1000_tokens: measurement_usize(sample_1000.pack_json_bytes),
            pack_text_bytes_for_1000_tokens: measurement_usize(sample_1000.pack_text_bytes),
            tokens_used_per_pack_for_1000_tokens: measurement_u32(sample_1000.tokens_used_per_pack),
            bytes_per_token_for_1000_tokens: measurement_f64(sample_1000.bytes_per_token),
            pack_json_bytes_for_2000_tokens: measurement_usize(sample_2000.pack_json_bytes),
            pack_text_bytes_for_2000_tokens: measurement_usize(sample_2000.pack_text_bytes),
            tokens_used_per_pack_for_2000_tokens: measurement_u32(sample_2000.tokens_used_per_pack),
            bytes_per_token_for_2000_tokens: measurement_f64(sample_2000.bytes_per_token),
            pack_json_bytes_for_4000_tokens: measurement_usize(sample_4000.pack_json_bytes),
            pack_text_bytes_for_4000_tokens: measurement_usize(sample_4000.pack_text_bytes),
            tokens_used_per_pack_for_4000_tokens: measurement_u32(sample_4000.tokens_used_per_pack),
            bytes_per_token_for_4000_tokens: measurement_f64(sample_4000.bytes_per_token),
            bytes_per_token: measurement_f64(sample_1000.bytes_per_token),
        })
    }
}

fn parse_corpus_records() -> Result<Vec<CorpusRecord>, String> {
    let mut records = Vec::new();
    for (line_index, line) in CORPUS_JSONL.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str::<CorpusRecord>(line)
            .map_err(|error| format!("parse corpus line {}: {error}", line_index + 1))?;
        records.push(record);
    }
    Ok(records)
}

fn default_confidence() -> f32 {
    0.8
}

fn measurement_usize(value: usize) -> PackSizeMeasurement {
    PackSizeMeasurement {
        value: value as f64,
        n: 1,
    }
}

fn measurement_u32(value: u32) -> PackSizeMeasurement {
    PackSizeMeasurement {
        value: f64::from(value),
        n: 1,
    }
}

fn measurement_f64(value: f64) -> PackSizeMeasurement {
    PackSizeMeasurement { value, n: 1 }
}

fn summary_path() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"))
        .join(SUMMARY_RELATIVE_PATH)
}

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::{
        CORPUS_FIXTURE_PATH, PACK_QUERY, PackSizeFixture, TOKEN_BUDGETS, parse_corpus_records,
    };

    #[test]
    fn corpus_fixture_has_expected_size() -> Result<(), String> {
        let records = parse_corpus_records()?;
        assert_eq!(records.len(), 15, "J2 corpus has 15 memory records");
        Ok(())
    }

    #[test]
    fn token_budget_set_matches_j5_acceptance() {
        assert_eq!(TOKEN_BUDGETS, &[500, 1000, 2000, 4000]);
    }

    #[test]
    fn summary_contains_required_measurements() -> Result<(), String> {
        let fixture = PackSizeFixture::prepare()?;
        let summary = fixture.summary()?;
        assert_eq!(summary.schema, "ee.bench.pack_size.summary.v1");
        assert_eq!(summary.corpus.fixture, CORPUS_FIXTURE_PATH);
        assert_eq!(summary.query, PACK_QUERY);
        assert!(summary.measurements.pack_json_bytes_for_1000_tokens.value > 0.0);
        assert!(summary.measurements.pack_text_bytes_for_1000_tokens.value > 0.0);
        assert!(
            summary
                .measurements
                .tokens_used_per_pack_for_1000_tokens
                .value
                > 0.0
        );
        assert!(summary.measurements.bytes_per_token.value > 0.0);
        Ok(())
    }
}
