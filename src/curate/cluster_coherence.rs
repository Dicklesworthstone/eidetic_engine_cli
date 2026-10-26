//! Deterministic cluster-coherence scoring for curation proposals.
//!
//! The learning pipeline can observe repeated memories before it has enough
//! validated evidence to promote a procedural rule. This module keeps the
//! clustering math explicit and testable: inputs are sorted by memory ID,
//! average-linkage agglomeration is deterministic, and every emitted cluster
//! carries the threshold and member set that determined it.

#[path = "cluster_coherence_merge.rs"]
mod merge;

#[path = "cluster_coherence_cached.rs"]
mod cached;

use serde::{Serialize, Serializer};

use crate::core::degraded_aggregation::{
    AggregatedDegradation, DegradationAggregationInput, aggregate_degraded_entries,
};

pub const DEFAULT_CLUSTER_COHERENCE_THRESHOLD: f64 = 0.55;
pub const DEFAULT_CLUSTER_SILHOUETTE_CUTOFF: f64 = 0.40;
pub const DEFAULT_MIN_CLUSTER_SIZE: usize = 3;
pub const CLUSTERING_INSUFFICIENT_DATA_CODE: &str = "clustering_insufficient_data";
pub const CLUSTERING_SINGLETON_SILHOUETTE_UNDEFINED_CODE: &str =
    "clustering_silhouette_undefined_for_singleton";
pub const CLUSTERING_THRESHOLD_TOO_STRICT_CODE: &str = "clustering_threshold_too_strict";

const FLOAT_EPSILON: f64 = 1.0e-12;

#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddingPoint {
    pub memory_id: String,
    pub embedding: Vec<f64>,
}

impl EmbeddingPoint {
    #[must_use]
    pub fn new(memory_id: impl Into<String>, embedding: Vec<f64>) -> Self {
        Self {
            memory_id: memory_id.into(),
            embedding,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClusterCoherenceConfig {
    pub merge_threshold: f64,
    pub silhouette_cutoff: f64,
    pub min_cluster_size: usize,
}

impl Default for ClusterCoherenceConfig {
    fn default() -> Self {
        Self {
            merge_threshold: DEFAULT_CLUSTER_COHERENCE_THRESHOLD,
            silhouette_cutoff: DEFAULT_CLUSTER_SILHOUETTE_CUTOFF,
            min_cluster_size: DEFAULT_MIN_CLUSTER_SIZE,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterCoherenceReport {
    pub method: String,
    pub threshold_used: f64,
    pub silhouette_cutoff: f64,
    pub min_cluster_size: usize,
    pub input_count: usize,
    pub clusters: Vec<CoherentCluster>,
    #[serde(serialize_with = "serialize_cluster_coherence_degraded")]
    pub degraded: Vec<ClusterCoherenceDegradation>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoherentCluster {
    pub cluster_id: String,
    pub representative_memory_id: String,
    pub member_memory_ids: Vec<String>,
    pub member_count: usize,
    pub average_internal_similarity: f64,
    pub nearest_external_similarity: Option<f64>,
    pub silhouette_score: Option<f64>,
    pub threshold_used: f64,
    pub accepted: bool,
    pub centroid_hash: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterCoherenceDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
}

fn serialize_cluster_coherence_degraded<S>(
    degraded: &[ClusterCoherenceDegradation],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    aggregate_cluster_coherence_degraded(degraded).serialize(serializer)
}

fn aggregate_cluster_coherence_degraded(
    degraded: &[ClusterCoherenceDegradation],
) -> Vec<AggregatedDegradation> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "cluster_coherence",
            entry.code.clone(),
            entry.severity.clone(),
            entry.message.clone(),
            entry.repair.clone(),
        )
    }))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClusterCoherenceError {
    message: String,
}

impl ClusterCoherenceError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ClusterCoherenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ClusterCoherenceError {}

#[must_use]
pub fn default_cluster_coherence_config() -> ClusterCoherenceConfig {
    ClusterCoherenceConfig::default()
}

pub fn agglomerate(
    points: &[EmbeddingPoint],
    config: ClusterCoherenceConfig,
) -> Result<ClusterCoherenceReport, ClusterCoherenceError> {
    analyze_embedding_clusters(points, config)
}

pub fn analyze_embedding_clusters(
    points: &[EmbeddingPoint],
    config: ClusterCoherenceConfig,
) -> Result<ClusterCoherenceReport, ClusterCoherenceError> {
    validate_config(config)?;
    if points.is_empty() {
        return Ok(ClusterCoherenceReport {
            method: "average_linkage_agglomerative".to_owned(),
            threshold_used: round_metric(config.merge_threshold),
            silhouette_cutoff: round_metric(config.silhouette_cutoff),
            min_cluster_size: config.min_cluster_size,
            input_count: 0,
            clusters: Vec::new(),
            degraded: vec![degradation(
                CLUSTERING_INSUFFICIENT_DATA_CODE,
                "warning",
                "No embedding points were supplied for clustering.",
                "Collect at least three related memories before proposing a curation candidate.",
            )],
        });
    }

    let points = normalized_points(points)?;
    let similarities = pairwise_cosine_matrix(&points)?;
    let cluster_members = cached::cluster(&similarities, config.merge_threshold);

    let mut degraded = Vec::new();
    if points.len() == 1 {
        degraded.push(degradation(
            CLUSTERING_SINGLETON_SILHOUETTE_UNDEFINED_CODE,
            "warning",
            "Only one embedding point was supplied; silhouette is undefined.",
            "Collect at least three related memories before promoting the cluster.",
        ));
    }

    let mut clusters = cluster_members
        .iter()
        .map(|members| coherent_cluster(members, &cluster_members, &points, &similarities, config))
        .collect::<Vec<_>>();
    clusters.sort_by(|left, right| {
        left.representative_memory_id
            .cmp(&right.representative_memory_id)
            .then_with(|| left.cluster_id.cmp(&right.cluster_id))
    });

    if !clusters.is_empty()
        && clusters
            .iter()
            .all(|cluster| cluster.member_count < config.min_cluster_size)
        && points.len() >= config.min_cluster_size
    {
        degraded.push(degradation(
            CLUSTERING_THRESHOLD_TOO_STRICT_CODE,
            "warning",
            "No cluster reached the configured minimum member count.",
            "Lower learn.cluster_coherence_threshold or collect stronger related evidence.",
        ));
    }

    Ok(ClusterCoherenceReport {
        method: "average_linkage_agglomerative".to_owned(),
        threshold_used: round_metric(config.merge_threshold),
        silhouette_cutoff: round_metric(config.silhouette_cutoff),
        min_cluster_size: config.min_cluster_size,
        input_count: points.len(),
        clusters,
        degraded,
    })
}

fn validate_config(config: ClusterCoherenceConfig) -> Result<(), ClusterCoherenceError> {
    if !(0.0..=1.0).contains(&config.merge_threshold) || !config.merge_threshold.is_finite() {
        return Err(ClusterCoherenceError::new(
            "cluster merge threshold must be finite and between 0.0 and 1.0",
        ));
    }
    if !(0.0..=1.0).contains(&config.silhouette_cutoff) || !config.silhouette_cutoff.is_finite() {
        return Err(ClusterCoherenceError::new(
            "cluster silhouette cutoff must be finite and between 0.0 and 1.0",
        ));
    }
    if config.min_cluster_size == 0 {
        return Err(ClusterCoherenceError::new(
            "cluster min_cluster_size must be at least 1",
        ));
    }
    Ok(())
}

fn normalized_points(
    points: &[EmbeddingPoint],
) -> Result<Vec<EmbeddingPoint>, ClusterCoherenceError> {
    let dimension = points
        .first()
        .map(|point| point.embedding.len())
        .unwrap_or_default();
    if dimension == 0 {
        return Err(ClusterCoherenceError::new(
            "cluster embeddings must not be empty",
        ));
    }
    let mut sorted = points.to_vec();
    sorted.sort_by(|left, right| left.memory_id.cmp(&right.memory_id));
    // A support threshold counts memories, not occurrences in a joined or
    // retried input. Refuse conflicting copies as well as identical copies;
    // choosing one would make evidence and centroid identity input-dependent.
    if sorted
        .windows(2)
        .any(|pair| pair[0].memory_id == pair[1].memory_id)
    {
        return Err(ClusterCoherenceError::new(
            "cluster memory IDs must be unique",
        ));
    }
    for point in &sorted {
        if point.memory_id.trim().is_empty() {
            return Err(ClusterCoherenceError::new(
                "cluster memory IDs must not be empty",
            ));
        }
        if point.embedding.len() != dimension {
            return Err(ClusterCoherenceError::new(
                "all cluster embeddings must have the same dimension",
            ));
        }
        if point.embedding.iter().any(|value| !value.is_finite()) {
            return Err(ClusterCoherenceError::new(
                "cluster embeddings must contain only finite values",
            ));
        }
        // Validate even a singleton: its matrix diagonal is assigned directly
        // and therefore never passes through the pairwise cosine validator.
        if point.embedding.iter().all(|value| *value == 0.0) {
            return Err(ClusterCoherenceError::new(
                "cluster embeddings must not be zero vectors",
            ));
        }
    }
    Ok(sorted)
}

fn pairwise_cosine_matrix(
    points: &[EmbeddingPoint],
) -> Result<Vec<Vec<f64>>, ClusterCoherenceError> {
    pairwise_cosine_matrix_with_norms(points, CosineNorm::new)
}

fn pairwise_cosine_matrix_with_norms(
    points: &[EmbeddingPoint],
    mut prepare: impl FnMut(&[f64]) -> Result<CosineNorm, ClusterCoherenceError>,
) -> Result<Vec<Vec<f64>>, ClusterCoherenceError> {
    // Norms depend on a point, not on a pair. Prepare them exactly once and
    // validate every input before allocating the quadratic similarity matrix.
    let norms = points
        .iter()
        .map(|point| prepare(&point.embedding))
        .collect::<Result<Vec<_>, _>>()?;
    let mut matrix = vec![vec![0.0; points.len()]; points.len()];
    for left in 0..points.len() {
        for right in left..points.len() {
            let similarity = if left == right {
                1.0
            } else {
                cosine_similarity(
                    &points[left].embedding,
                    &points[right].embedding,
                    norms[left],
                    norms[right],
                )?
            };
            matrix[left][right] = similarity;
            matrix[right][left] = similarity;
        }
    }
    Ok(matrix)
}

#[derive(Clone, Copy)]
struct CosineNorm {
    ordinary: Option<f64>,
    scale: f64,
    scaled_length: f64,
}

impl CosineNorm {
    fn new(values: &[f64]) -> Result<Self, ClusterCoherenceError> {
        if values.iter().any(|value| !value.is_finite()) {
            return Err(ClusterCoherenceError::new(
                "cluster embeddings must contain only finite values",
            ));
        }
        let scale = values
            .iter()
            .fold(0.0_f64, |scale, value| scale.max(value.abs()));
        if scale == 0.0 {
            return Err(ClusterCoherenceError::new(
                "cluster embeddings must not be zero vectors",
            ));
        }
        let squared = values.iter().map(|value| value * value).sum::<f64>();
        let scaled_length = values
            .iter()
            .map(|value| {
                let scaled = value / scale;
                scaled * scaled
            })
            .sum::<f64>()
            .sqrt();
        Ok(Self {
            // A subnormal/overflowed sum is not a reliable norm. Nonzero
            // direction, unlike magnitude, is necessary for cosine scoring.
            ordinary: squared.is_normal().then(|| squared.sqrt()),
            scale,
            scaled_length,
        })
    }
}

fn cosine_similarity(
    left: &[f64],
    right: &[f64],
    left_norm: CosineNorm,
    right_norm: CosineNorm,
) -> Result<f64, ClusterCoherenceError> {
    if let Some((left_length, right_length)) = left_norm.ordinary.zip(right_norm.ordinary) {
        let denominator = left_length * right_length;
        if denominator.is_normal() {
            // Retain the exact ordinary arithmetic and rounding order. The
            // scaled lane is only for numerically unsafe intermediate values.
            let dot = left
                .iter()
                .zip(right.iter())
                .map(|(left, right)| left * right)
                .sum::<f64>();
            let score = dot / denominator;
            if score.is_finite() {
                return Ok(score.clamp(-1.0, 1.0));
            }
        }
    }
    let dot = left
        .iter()
        .zip(right.iter())
        .map(|(left, right)| (left / left_norm.scale) * (right / right_norm.scale))
        .sum::<f64>();
    let score = dot / (left_norm.scaled_length * right_norm.scaled_length);
    if !score.is_finite() {
        return Err(ClusterCoherenceError::new(
            "cluster cosine computation must produce a finite value",
        ));
    }
    Ok(score.clamp(-1.0, 1.0))
}

fn coherent_cluster(
    members: &[usize],
    all_clusters: &[Vec<usize>],
    points: &[EmbeddingPoint],
    similarities: &[Vec<f64>],
    config: ClusterCoherenceConfig,
) -> CoherentCluster {
    let representative_memory_id = members
        .iter()
        .map(|index| points[*index].memory_id.as_str())
        .min()
        .unwrap_or_default()
        .to_owned();
    let member_memory_ids = members
        .iter()
        .map(|index| points[*index].memory_id.clone())
        .collect::<Vec<_>>();
    let average_internal_similarity = internal_similarity(members, similarities);
    // This cluster-level summary is explanatory, not the per-member nearest
    // alternative used by the silhouette calculation below.
    let nearest_external_similarity_raw = all_clusters
        .iter()
        .filter(|cluster| cluster.as_slice() != members)
        .map(|cluster| average_linkage_similarity(members, cluster, similarities))
        .max_by(f64::total_cmp);
    let silhouette_raw = silhouette_score(members, all_clusters, similarities);
    let silhouette_score = silhouette_raw.map(round_metric);
    let nearest_external_similarity = nearest_external_similarity_raw.map(round_metric);
    let accepted = members.len() >= config.min_cluster_size
        && silhouette_raw.is_some_and(|score| score >= config.silhouette_cutoff);
    let cluster_id = cluster_id(&member_memory_ids, config.merge_threshold);
    let centroid_hash = centroid_hash(members, points);
    CoherentCluster {
        cluster_id,
        representative_memory_id,
        member_memory_ids,
        member_count: members.len(),
        average_internal_similarity: round_metric(average_internal_similarity),
        nearest_external_similarity,
        silhouette_score,
        threshold_used: round_metric(config.merge_threshold),
        accepted,
        centroid_hash,
    }
}

fn average_linkage_similarity(left: &[usize], right: &[usize], similarities: &[Vec<f64>]) -> f64 {
    let mut total = 0.0;
    let mut count = 0_usize;
    for left_index in left {
        for right_index in right {
            total += similarities[*left_index][*right_index];
            count = count.saturating_add(1);
        }
    }
    if count == 0 {
        0.0
    } else {
        total / count as f64
    }
}

fn internal_similarity(members: &[usize], similarities: &[Vec<f64>]) -> f64 {
    if members.len() < 2 {
        return 1.0;
    }
    let mut total = 0.0;
    let mut count = 0_usize;
    for left in 0..members.len() {
        for right in (left + 1)..members.len() {
            total += similarities[members[left]][members[right]];
            count = count.saturating_add(1);
        }
    }
    total / count as f64
}

fn silhouette_score(
    members: &[usize],
    all_clusters: &[Vec<usize>],
    similarities: &[Vec<f64>],
) -> Option<f64> {
    if members.len() < 2 {
        return None;
    }
    let alternatives = all_clusters
        .iter()
        .filter(|cluster| !cluster.is_empty() && cluster.as_slice() != members)
        .collect::<Vec<_>>();
    if alternatives.is_empty() {
        // Retain the existing single-cluster admission convention. There is
        // no measured separation in this case; nearestExternalSimilarity
        // remains null. Multi-cluster scores below use cosine distances.
        return Some(1.0);
    }
    let mut total = 0.0;
    for member in members {
        // a(i) excludes the point itself; b(i) chooses the nearest OTHER
        // cluster separately for each point. Averaging cosine similarities
        // first, or dividing by similarities instead of distances, does not
        // compute a silhouette and rejects compact, nearby topics.
        let within = members
            .iter()
            .filter(|other| *other != member)
            .map(|other| 1.0 - similarities[*member][*other])
            .sum::<f64>()
            / (members.len() - 1) as f64;
        let nearest = alternatives
            .iter()
            .map(|cluster| {
                cluster
                    .iter()
                    .map(|other| 1.0 - similarities[*member][*other])
                    .sum::<f64>()
                    / cluster.len() as f64
            })
            .min_by(f64::total_cmp)?;
        let denominator = within.max(nearest);
        // Coincident points in different clusters have zero separation.
        // Do not use an absolute epsilon: cosine distance can legitimately
        // be tiny, and its ratio still contains the separation signal.
        if denominator > 0.0 {
            total += ((nearest - within) / denominator).clamp(-1.0, 1.0);
        }
    }
    Some((total / members.len() as f64).clamp(-1.0, 1.0))
}

fn cluster_id(member_memory_ids: &[String], threshold: f64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.cluster_coherence.v1\n");
    hash_text(&mut hasher, "threshold", &format!("{threshold:.6}"));
    for memory_id in member_memory_ids {
        hash_text(&mut hasher, "member", memory_id);
    }
    let digest = hasher.finalize().to_hex().to_string();
    format!("clu_{}", &digest[..digest.len().min(24)])
}

fn centroid_hash(members: &[usize], points: &[EmbeddingPoint]) -> String {
    let dimension = points
        .first()
        .map(|point| point.embedding.len())
        .unwrap_or_default();
    let mut centroid = vec![0.0; dimension];
    for member in members {
        for (target, value) in centroid.iter_mut().zip(points[*member].embedding.iter()) {
            *target += value;
        }
    }
    if !members.is_empty() {
        for (coordinate, value) in centroid.iter_mut().enumerate() {
            if value.is_finite() {
                *value /= members.len() as f64;
            } else {
                // A mean of finite inputs is finite even when its sum is not.
                // Preserve historical bytes for ordinary means, but never
                // collapse unrelated large centroids into the same inf/NaN tag.
                *value = scaled_centroid_coordinate(members, points, coordinate);
            }
        }
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.cluster_centroid.v1\n");
    for value in centroid {
        hash_text(&mut hasher, "value", &format!("{value:.9}"));
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn scaled_centroid_coordinate(
    members: &[usize],
    points: &[EmbeddingPoint],
    coordinate: usize,
) -> f64 {
    let scale = members.iter().fold(0.0_f64, |scale, member| {
        scale.max(points[*member].embedding[coordinate].abs())
    });
    if scale == 0.0 {
        return 0.0;
    }
    let total = members
        .iter()
        .map(|member| points[*member].embedding[coordinate] / scale)
        .sum::<f64>();
    // Rounding may not move a mean outside its inputs' magnitude bound.
    (total / members.len() as f64).clamp(-1.0, 1.0) * scale
}

fn hash_text(hasher: &mut blake3::Hasher, field: &str, value: &str) {
    hasher.update(field.as_bytes());
    hasher.update(b"\0");
    hasher.update(value.len().to_string().as_bytes());
    hasher.update(b":");
    hasher.update(value.as_bytes());
    hasher.update(b"\n");
}

fn degradation(
    code: &str,
    severity: &str,
    message: &str,
    repair: &str,
) -> ClusterCoherenceDegradation {
    ClusterCoherenceDegradation {
        code: code.to_owned(),
        severity: severity.to_owned(),
        message: message.to_owned(),
        repair: repair.to_owned(),
    }
}

fn round_metric(value: f64) -> f64 {
    if value.is_finite() {
        (value * 1000.0).round() / 1000.0
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_nonzero_directions_survive_extreme_and_subnormal_magnitudes() {
        let scales = [
            f64::from_bits(1),
            f64::MIN_POSITIVE,
            1.0e-200,
            1.0e-14,
            1.0,
            1.0e200,
            f64::MAX,
        ];
        for left in scales {
            for right in scales {
                let points = vec![
                    EmbeddingPoint::new("a", vec![left, 0.0]),
                    EmbeddingPoint::new("b", vec![right, 0.0]),
                    EmbeddingPoint::new("c", vec![-right, 0.0]),
                    EmbeddingPoint::new("d", vec![0.0, right]),
                ];
                let matrix = pairwise_cosine_matrix(&points).unwrap();
                assert!((matrix[0][1] - 1.0).abs() < 1.0e-12);
                assert!((matrix[0][2] + 1.0).abs() < 1.0e-12);
                assert!(matrix[0][3].abs() < 1.0e-12);
                for row in 0..matrix.len() {
                    for column in 0..matrix.len() {
                        assert!(matrix[row][column].is_finite());
                        assert_eq!(matrix[row][column], matrix[column][row]);
                    }
                }
            }
        }
    }

    #[test]
    fn overflowing_products_do_not_turn_parallel_vectors_into_unrelated_topics() {
        let points = vec![
            EmbeddingPoint::new("a", vec![f64::MAX, f64::MAX]),
            EmbeddingPoint::new("b", vec![f64::MAX, f64::MAX]),
            EmbeddingPoint::new("c", vec![f64::MAX, -f64::MAX]),
        ];
        let matrix = pairwise_cosine_matrix(&points).unwrap();
        assert!((matrix[0][1] - 1.0).abs() < 1.0e-12);
        assert!(matrix[0][2].abs() < 1.0e-12);
        assert!(matrix[1][2].abs() < 1.0e-12);
    }

    #[test]
    fn positive_rescaling_preserves_topic_membership_scores_and_admission() {
        let original = angular_points(&[0.0, 5.0, 10.0, 45.0, 50.0, 55.0]);
        let config = ClusterCoherenceConfig {
            merge_threshold: 0.95,
            ..ClusterCoherenceConfig::default()
        };
        let baseline = agglomerate(&original, config).unwrap();
        let mut scaled = original.clone();
        for (point, scale) in scaled
            .iter_mut()
            .zip([1.0e300, 1.0e-300, 1.0e200, 1.0e-200, 1.0e-14, 1.0])
        {
            for value in &mut point.embedding {
                *value *= scale;
            }
        }
        let unchanged = scaled.clone();
        let report = agglomerate(&scaled, config).unwrap();
        assert_eq!(
            scaled, unchanged,
            "scoring may not rewrite caller embeddings"
        );
        assert_eq!(report.clusters.len(), baseline.clusters.len());
        for (actual, expected) in report.clusters.iter().zip(&baseline.clusters) {
            assert_eq!(actual.cluster_id, expected.cluster_id);
            assert_eq!(actual.member_memory_ids, expected.member_memory_ids);
            assert_eq!(
                actual.average_internal_similarity,
                expected.average_internal_similarity
            );
            assert_eq!(
                actual.nearest_external_similarity,
                expected.nearest_external_similarity
            );
            assert_eq!(actual.silhouette_score, expected.silhouette_score);
            assert_eq!(actual.accepted, expected.accepted);
        }
        scaled.reverse();
        assert_eq!(agglomerate(&scaled, config).unwrap(), report);
    }

    #[test]
    fn ordinary_pair_scores_and_centroid_commitments_retain_their_original_bytes() {
        let points = angular_points(&[-170.0, -90.0, -5.0, 0.0, 5.0, 55.0, 120.0, 170.0]);
        let matrix = pairwise_cosine_matrix(&points).unwrap();
        for left in 0..points.len() {
            for right in left + 1..points.len() {
                // Literal pre-change arithmetic is the compatibility oracle.
                let a = &points[left].embedding;
                let b = &points[right].embedding;
                let dot = a.iter().zip(b).map(|(a, b)| a * b).sum::<f64>();
                let a_norm = a.iter().map(|v| v * v).sum::<f64>().sqrt();
                let b_norm = b.iter().map(|v| v * v).sum::<f64>().sqrt();
                let expected = (dot / (a_norm * b_norm)).clamp(-1.0, 1.0);
                assert_eq!(matrix[left][right].to_bits(), expected.to_bits());
            }
        }
        let members = vec![0, 1, 2];
        let mut expected = blake3::Hasher::new();
        expected.update(b"ee.cluster_centroid.v1\n");
        for coordinate in 0..2 {
            let mean = members
                .iter()
                .map(|member| points[*member].embedding[coordinate])
                .sum::<f64>()
                / members.len() as f64;
            hash_text(&mut expected, "value", &format!("{mean:.9}"));
        }
        assert_eq!(
            centroid_hash(&members, &points),
            format!("blake3:{}", expected.finalize().to_hex())
        );
    }

    #[test]
    fn large_centroids_commit_finite_means_instead_of_a_shared_infinity_tag() {
        let mut hashes = Vec::new();
        for value in [f64::MAX, f64::MAX / 2.0, -f64::MAX] {
            let points = (0..3)
                .map(|index| EmbeddingPoint::new(format!("mem_{index}"), vec![value]))
                .collect::<Vec<_>>();
            let report = agglomerate(&points, ClusterCoherenceConfig::default()).unwrap();
            assert_eq!(report.clusters.len(), 1);
            assert!(report.clusters[0].accepted);
            let mut expected = blake3::Hasher::new();
            expected.update(b"ee.cluster_centroid.v1\n");
            hash_text(&mut expected, "value", &format!("{value:.9}"));
            assert_eq!(
                report.clusters[0].centroid_hash,
                format!("blake3:{}", expected.finalize().to_hex())
            );
            hashes.push(report.clusters[0].centroid_hash.clone());
        }
        hashes.sort();
        hashes.dedup();
        assert_eq!(hashes.len(), 3);
        let points = vec![
            EmbeddingPoint::new("a", vec![f64::MAX]),
            EmbeddingPoint::new("b", vec![f64::MAX]),
            EmbeddingPoint::new("c", vec![-f64::MAX]),
        ];
        let mean = scaled_centroid_coordinate(&[0, 1, 2], &points, 0);
        assert!(mean.is_finite());
        assert!((mean / f64::MAX - 1.0 / 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn norm_preparation_is_linear_in_points_not_quadratic_in_pairs() {
        let points = angular_points(&[0.0, 5.0, 10.0, 45.0, 50.0, 55.0]);
        let mut calls = 0;
        let observed = pairwise_cosine_matrix_with_norms(&points, |values| {
            calls += 1;
            CosineNorm::new(values)
        })
        .unwrap();
        assert_eq!(calls, points.len());
        assert_eq!(observed, pairwise_cosine_matrix(&points).unwrap());
        for value in [0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let invalid = vec![EmbeddingPoint::new("invalid_singleton", vec![value])];
            assert!(pairwise_cosine_matrix(&invalid).is_err());
        }
    }

    #[test]
    fn repeated_memory_cannot_manufacture_minimum_cluster_support() {
        let point = EmbeddingPoint::new("mem_one_observation", vec![1.0, 0.0]);
        let points = vec![point; DEFAULT_MIN_CLUSTER_SIZE];
        let error = agglomerate(&points, ClusterCoherenceConfig::default()).unwrap_err();
        assert_eq!(error.message(), "cluster memory IDs must be unique");
        assert!(!error.message().contains("mem_one_observation"));
    }

    #[test]
    fn conflicting_duplicate_identity_is_refused_in_every_input_order() {
        let original = vec![
            EmbeddingPoint::new("mem_a", vec![1.0, 0.0]),
            EmbeddingPoint::new("mem_b", vec![0.0, 1.0]),
            EmbeddingPoint::new("mem_a", vec![-1.0, 0.0]),
        ];
        for reverse in [false, true] {
            for offset in 0..original.len() {
                let mut points = original.clone();
                if reverse {
                    points.reverse();
                }
                points.rotate_left(offset);
                let error = agglomerate(&points, ClusterCoherenceConfig::default()).unwrap_err();
                assert_eq!(error.message(), "cluster memory IDs must be unique");
            }
        }
    }

    #[test]
    fn distinct_memories_with_identical_vectors_keep_their_support_and_identity() {
        let points = (0..DEFAULT_MIN_CLUSTER_SIZE)
            .map(|index| EmbeddingPoint::new(format!("mem_{index}"), vec![1.0, 0.0]))
            .collect::<Vec<_>>();
        let report = agglomerate(&points, ClusterCoherenceConfig::default()).unwrap();
        assert_eq!(report.input_count, points.len());
        assert_eq!(report.clusters.len(), 1);
        let cluster = &report.clusters[0];
        assert!(cluster.accepted);
        assert_eq!(cluster.member_count, points.len());
        assert_eq!(
            cluster.member_memory_ids,
            points
                .iter()
                .map(|point| point.memory_id.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn zero_vectors_are_invalid_even_when_no_pairwise_comparison_runs() {
        for embedding in [vec![0.0], vec![-0.0, 0.0]] {
            let point = EmbeddingPoint::new("mem_zero", embedding);
            let error = agglomerate(
                std::slice::from_ref(&point),
                ClusterCoherenceConfig::default(),
            )
            .unwrap_err();
            assert_eq!(
                error.message(),
                "cluster embeddings must not be zero vectors"
            );
            let mut points = vec![
                EmbeddingPoint::new("mem_valid", vec![1.0; point.embedding.len()]),
                point,
            ];
            for _ in 0..2 {
                points.reverse();
                assert!(agglomerate(&points, ClusterCoherenceConfig::default()).is_err());
            }
        }
    }

    fn angular_points(angles: &[f64]) -> Vec<EmbeddingPoint> {
        angles
            .iter()
            .enumerate()
            .map(|(index, angle)| {
                let radians = angle.to_radians();
                EmbeddingPoint::new(
                    format!("memory_{index:02}"),
                    vec![radians.cos(), radians.sin()],
                )
            })
            .collect()
    }

    #[test]
    fn compact_nearby_topics_are_admitted_using_cosine_distance_silhouettes() {
        let points = angular_points(&[0.0, 5.0, 10.0, 45.0, 50.0, 55.0]);
        let report = agglomerate(
            &points,
            ClusterCoherenceConfig {
                merge_threshold: 0.95,
                ..ClusterCoherenceConfig::default()
            },
        )
        .unwrap();
        assert_eq!(report.clusters.len(), 2);
        for cluster in &report.clusters {
            assert_eq!(cluster.member_count, 3);
            assert!((cluster.silhouette_score.unwrap() - 0.973).abs() < 1.0e-9);
            assert!(cluster.accepted);
        }
    }

    #[test]
    fn nearest_alternative_is_chosen_per_member_not_per_cluster() {
        let points = angular_points(&[-10.0, 10.0, -11.0, 11.0]);
        let similarities = pairwise_cosine_matrix(&points).unwrap();
        let clusters = vec![vec![0, 1], vec![2], vec![3]];
        let score = silhouette_score(&clusters[0], &clusters, &similarities).unwrap();
        let expected =
            (1.0 - 1.0_f64.to_radians().cos()) / (1.0 - 20.0_f64.to_radians().cos()) - 1.0;
        assert!((score - expected).abs() < 1.0e-12);
        assert!(score < -0.99);
    }

    #[test]
    fn admission_uses_unrounded_silhouette_not_display_precision() {
        let external = 1.0 - 0.5 / (1.0 - 0.3996);
        let similarities = vec![
            vec![1.0, 0.5, external],
            vec![0.5, 1.0, external],
            vec![external, external, 1.0],
        ];
        let points = angular_points(&[0.0, 60.0, 120.0]);
        let clusters = vec![vec![0, 1], vec![2]];
        let cluster = coherent_cluster(
            &clusters[0],
            &clusters,
            &points,
            &similarities,
            ClusterCoherenceConfig {
                min_cluster_size: 2,
                ..ClusterCoherenceConfig::default()
            },
        );
        assert!((cluster.silhouette_score.unwrap() - 0.4).abs() < 1.0e-12);
        assert!(!cluster.accepted, "rounding must not manufacture admission");
    }

    #[test]
    fn coincident_alternatives_have_zero_silhouette_without_nan() {
        let similarities = vec![vec![1.0; 3]; 3];
        let clusters = vec![vec![0, 1], vec![2]];
        let score = silhouette_score(&clusters[0], &clusters, &similarities).unwrap();
        assert!(score.is_finite() && score.abs() < f64::EPSILON);
        assert!(silhouette_score(&clusters[1], &clusters, &similarities).is_none());
    }

    #[test]
    fn silhouette_and_admission_remain_stable_under_input_permutation() {
        let mut points = angular_points(&[0.0, 5.0, 10.0, 45.0, 50.0, 55.0]);
        let config = ClusterCoherenceConfig {
            merge_threshold: 0.95,
            ..ClusterCoherenceConfig::default()
        };
        let forward = agglomerate(&points, config).unwrap();
        points.reverse();
        assert_eq!(agglomerate(&points, config).unwrap(), forward);
    }

    #[test]
    fn single_cluster_retains_existing_admission_convention() {
        let points = angular_points(&[0.0, 5.0, 10.0]);
        let report = agglomerate(&points, ClusterCoherenceConfig::default()).unwrap();
        assert_eq!(report.clusters.len(), 1);
        let cluster = &report.clusters[0];
        assert!(cluster.nearest_external_similarity.is_none());
        assert!((cluster.silhouette_score.unwrap() - 1.0).abs() < f64::EPSILON);
        assert!(cluster.accepted);
    }

    #[test]
    fn cluster_coherence_serializes_aggregated_degraded_entries() -> Result<(), String> {
        let mut first = degradation(
            CLUSTERING_INSUFFICIENT_DATA_CODE,
            "warning",
            "first insufficient-data warning",
            "Collect at least three related memories before proposing a curation candidate.",
        );
        let mut second = degradation(
            CLUSTERING_INSUFFICIENT_DATA_CODE,
            "warning",
            "second insufficient-data warning",
            "Collect at least three related memories before proposing a curation candidate.",
        );
        first.repair = "Collect more related memories.".to_owned();
        second.repair = "Collect more related memories.".to_owned();

        let report = ClusterCoherenceReport {
            method: "average_linkage_agglomerative".to_owned(),
            threshold_used: 0.55,
            silhouette_cutoff: 0.4,
            min_cluster_size: 3,
            input_count: 0,
            clusters: Vec::new(),
            degraded: vec![first, second],
        };

        let value = serde_json::to_value(report).map_err(|error| error.to_string())?;
        let degraded = value
            .get("degraded")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                "serialized cluster coherence should include degraded array".to_owned()
            })?;

        assert_eq!(degraded.len(), 1);
        assert_eq!(
            degraded[0].get("code"),
            Some(&serde_json::json!(CLUSTERING_INSUFFICIENT_DATA_CODE))
        );
        assert_eq!(
            degraded[0].get("severity"),
            Some(&serde_json::json!("warning"))
        );
        assert_eq!(
            degraded[0].get("repair"),
            Some(&serde_json::json!("Collect more related memories."))
        );
        assert_eq!(
            degraded[0].get("sources"),
            Some(&serde_json::json!(["cluster_coherence"]))
        );
        Ok(())
    }
}
