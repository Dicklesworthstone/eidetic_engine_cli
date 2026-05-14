use std::collections::BTreeMap;

use fnx_runtime::CgseValue;
use serde::{Deserialize, Serialize};

pub const DEFAULT_SAMPLE_THRESHOLD: usize = 500;
pub const DEFAULT_SAMPLE_SIZE: usize = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingChoice {
    Exact,
    Approximate,
}

impl SamplingChoice {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Approximate => "approximate",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplingWitness {
    pub algorithm: String,
    pub snapshot_version: u64,
    pub node_count: usize,
    pub sample_threshold: usize,
    pub requested_sample_size: usize,
    pub effective_sample_size: usize,
    pub choice: SamplingChoice,
    pub seed: u64,
    pub pivots: Vec<usize>,
    pub decision_path_hash: String,
}

impl SamplingWitness {
    #[must_use]
    pub fn to_cgse_value(&self) -> CgseValue {
        let mut fields = BTreeMap::new();
        fields.insert(
            "algorithm".to_owned(),
            CgseValue::String(self.algorithm.clone()),
        );
        fields.insert(
            "choice".to_owned(),
            CgseValue::String(self.choice.as_str().to_owned()),
        );
        fields.insert(
            "decisionPathHash".to_owned(),
            CgseValue::String(self.decision_path_hash.clone()),
        );
        fields.insert(
            "effectiveSampleSize".to_owned(),
            cgse_usize(self.effective_sample_size),
        );
        fields.insert("nodeCount".to_owned(), cgse_usize(self.node_count));
        fields.insert(
            "requestedSampleSize".to_owned(),
            cgse_usize(self.requested_sample_size),
        );
        fields.insert(
            "pivots".to_owned(),
            CgseValue::String(
                self.pivots
                    .iter()
                    .map(|pivot| pivot.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        );
        fields.insert(
            "sampleThreshold".to_owned(),
            cgse_usize(self.sample_threshold),
        );
        fields.insert("seed".to_owned(), cgse_u64(self.seed));
        fields.insert(
            "snapshotVersion".to_owned(),
            cgse_u64(self.snapshot_version),
        );
        CgseValue::Map(fields)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SamplingRun<R> {
    pub result: R,
    pub witness: SamplingWitness,
}

impl<R> SamplingRun<R> {
    #[must_use]
    pub fn into_result(self) -> R {
        self.result
    }
}

pub fn run_with_sampling<R, Exact, Approx>(
    name: &str,
    node_count: usize,
    sample_threshold: usize,
    sample_size: usize,
    snapshot_version: u64,
    f_exact: Exact,
    f_approx: Approx,
) -> SamplingRun<R>
where
    Exact: FnOnce() -> R,
    Approx: FnOnce(&[usize], u64) -> R,
{
    let seed = deterministic_sampling_seed(
        name,
        snapshot_version,
        node_count,
        sample_threshold,
        sample_size,
    );
    let choice = if node_count < sample_threshold {
        SamplingChoice::Exact
    } else {
        SamplingChoice::Approximate
    };
    let pivots = match choice {
        SamplingChoice::Exact => Vec::new(),
        SamplingChoice::Approximate => deterministic_sample_pivots(node_count, sample_size, seed),
    };
    let effective_sample_size = pivots.len();
    let decision_path_hash = sampling_decision_path_hash(&SamplingDecisionHashInput {
        name,
        snapshot_version,
        node_count,
        sample_threshold,
        sample_size,
        choice,
        seed,
        pivots: &pivots,
    });
    let witness = SamplingWitness {
        algorithm: name.to_owned(),
        snapshot_version,
        node_count,
        sample_threshold,
        requested_sample_size: sample_size,
        effective_sample_size,
        choice,
        seed,
        pivots,
        decision_path_hash,
    };
    let result = match choice {
        SamplingChoice::Exact => f_exact(),
        SamplingChoice::Approximate => f_approx(&witness.pivots, seed),
    };

    SamplingRun { result, witness }
}

#[must_use]
pub fn deterministic_sampling_seed(
    name: &str,
    snapshot_version: u64,
    node_count: usize,
    sample_threshold: usize,
    sample_size: usize,
) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.graph.algorithms.sampling.seed.v1");
    hasher.update(name.as_bytes());
    hasher.update(&snapshot_version.to_le_bytes());
    hasher.update(&node_count.to_le_bytes());
    hasher.update(&sample_threshold.to_le_bytes());
    hasher.update(&sample_size.to_le_bytes());
    let digest = hasher.finalize();
    let mut seed_bytes = [0_u8; 8];
    seed_bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(seed_bytes)
}

#[must_use]
pub fn deterministic_sample_pivots(node_count: usize, sample_size: usize, seed: u64) -> Vec<usize> {
    if node_count == 0 || sample_size == 0 {
        return Vec::new();
    }

    let effective_sample_size = sample_size.min(node_count);
    let mut ranked: Vec<(blake3::Hash, usize)> = (0..node_count)
        .map(|node_index| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"ee.graph.algorithms.sampling.pivot.v1");
            hasher.update(&seed.to_le_bytes());
            hasher.update(&node_index.to_le_bytes());
            (hasher.finalize(), node_index)
        })
        .collect();
    ranked.sort_by(|left, right| {
        left.0
            .as_bytes()
            .cmp(right.0.as_bytes())
            .then_with(|| left.1.cmp(&right.1))
    });

    let mut pivots: Vec<_> = ranked
        .into_iter()
        .take(effective_sample_size)
        .map(|(_, node_index)| node_index)
        .collect();
    pivots.sort_unstable();
    pivots
}

struct SamplingDecisionHashInput<'a> {
    name: &'a str,
    snapshot_version: u64,
    node_count: usize,
    sample_threshold: usize,
    sample_size: usize,
    choice: SamplingChoice,
    seed: u64,
    pivots: &'a [usize],
}

fn sampling_decision_path_hash(input: &SamplingDecisionHashInput<'_>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.graph.algorithms.sampling.decision.v1");
    hasher.update(input.name.as_bytes());
    hasher.update(input.choice.as_str().as_bytes());
    hasher.update(&input.snapshot_version.to_le_bytes());
    hasher.update(&input.node_count.to_le_bytes());
    hasher.update(&input.sample_threshold.to_le_bytes());
    hasher.update(&input.sample_size.to_le_bytes());
    hasher.update(&input.seed.to_le_bytes());
    for pivot in input.pivots {
        hasher.update(&pivot.to_le_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn cgse_usize(value: usize) -> CgseValue {
    match i64::try_from(value) {
        Ok(value) => CgseValue::Int(value),
        Err(_) => CgseValue::String(value.to_string()),
    }
}

fn cgse_u64(value: u64) -> CgseValue {
    match i64::try_from(value) {
        Ok(value) => CgseValue::Int(value),
        Err(_) => CgseValue::String(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_with_sampling_uses_exact_under_threshold() {
        let run = run_with_sampling(
            "betweenness",
            499,
            DEFAULT_SAMPLE_THRESHOLD,
            DEFAULT_SAMPLE_SIZE,
            7,
            || "exact",
            |_, _| "approx",
        );

        assert_eq!(run.result, "exact");
        assert_eq!(run.witness.choice, SamplingChoice::Exact);
        assert_eq!(run.witness.effective_sample_size, 0);
        assert!(run.witness.pivots.is_empty());
        assert!(run.witness.decision_path_hash.starts_with("blake3:"));
    }

    #[test]
    fn run_with_sampling_uses_approx_at_or_over_threshold() {
        let run = run_with_sampling(
            "betweenness",
            500,
            DEFAULT_SAMPLE_THRESHOLD,
            DEFAULT_SAMPLE_SIZE,
            7,
            || (0, 0),
            |pivots, seed| (pivots.len(), seed),
        );

        assert_eq!(run.witness.choice, SamplingChoice::Approximate);
        assert_eq!(run.result.0, DEFAULT_SAMPLE_SIZE);
        assert_eq!(run.result.1, run.witness.seed);
        assert_eq!(run.witness.pivots.len(), DEFAULT_SAMPLE_SIZE);
        assert!(run.witness.pivots.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn sampling_witness_is_recorded_as_deterministic_cgse_value() {
        let first = run_with_sampling(
            "k_truss",
            1_000,
            DEFAULT_SAMPLE_THRESHOLD,
            DEFAULT_SAMPLE_SIZE,
            11,
            || "exact",
            |pivots, seed| {
                assert_eq!(pivots.len(), DEFAULT_SAMPLE_SIZE);
                assert_ne!(seed, 0);
                "approx"
            },
        );
        let second = run_with_sampling(
            "k_truss",
            1_000,
            DEFAULT_SAMPLE_THRESHOLD,
            DEFAULT_SAMPLE_SIZE,
            11,
            || "exact",
            |_, _| "approx",
        );
        let different_snapshot = run_with_sampling(
            "k_truss",
            1_000,
            DEFAULT_SAMPLE_THRESHOLD,
            DEFAULT_SAMPLE_SIZE,
            12,
            || "exact",
            |_, _| "approx",
        );

        assert_eq!(first.result, "approx");
        assert_eq!(first.witness, second.witness);
        assert_ne!(first.witness.seed, different_snapshot.witness.seed);
        assert_ne!(
            first.witness.decision_path_hash,
            different_snapshot.witness.decision_path_hash
        );
        assert_eq!(first.witness.pivots, second.witness.pivots);

        let CgseValue::Map(fields) = first.witness.to_cgse_value() else {
            panic!("sampling witness should render as CGSE map");
        };
        assert_eq!(
            fields.get("choice"),
            Some(&CgseValue::String("approximate".to_owned()))
        );
        assert_eq!(fields.get("snapshotVersion"), Some(&CgseValue::Int(11)));
    }
}
