//! Deterministic cluster-coherence scoring for curation proposals.
//!
//! The learning pipeline can observe repeated memories before it has enough
//! validated evidence to promote a procedural rule. This module keeps the
//! clustering math explicit and testable: inputs are sorted by memory ID,
//! average-linkage agglomeration is deterministic, and every emitted cluster
//! carries the threshold and member set that determined it.

use std::cmp::Ordering;

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
    let mut cluster_members = (0..points.len())
        .map(|index| vec![index])
        .collect::<Vec<_>>();
    while let Some((left, right, _similarity)) =
        best_merge(&cluster_members, &similarities, config.merge_threshold)
    {
        let mut merged = cluster_members[left].clone();
        merged.extend(cluster_members[right].iter().copied());
        merged.sort_unstable();
        cluster_members[left] = merged;
        cluster_members.remove(right);
    }

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
    }
    Ok(sorted)
}

fn pairwise_cosine_matrix(
    points: &[EmbeddingPoint],
) -> Result<Vec<Vec<f64>>, ClusterCoherenceError> {
    let mut matrix = vec![vec![0.0; points.len()]; points.len()];
    for left in 0..points.len() {
        for right in left..points.len() {
            let similarity = if left == right {
                1.0
            } else {
                cosine_similarity(&points[left].embedding, &points[right].embedding)?
            };
            matrix[left][right] = similarity;
            matrix[right][left] = similarity;
        }
    }
    Ok(matrix)
}

fn cosine_similarity(left: &[f64], right: &[f64]) -> Result<f64, ClusterCoherenceError> {
    let dot = left
        .iter()
        .zip(right.iter())
        .map(|(left, right)| left * right)
        .sum::<f64>();
    let left_norm = left.iter().map(|value| value * value).sum::<f64>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f64>().sqrt();
    if left_norm <= FLOAT_EPSILON || right_norm <= FLOAT_EPSILON {
        return Err(ClusterCoherenceError::new(
            "cluster embeddings must not be zero vectors",
        ));
    }
    let score = dot / (left_norm * right_norm);
    if score.is_nan() {
        Ok(0.0)
    } else {
        Ok(score.clamp(-1.0, 1.0))
    }
}

fn best_merge(
    clusters: &[Vec<usize>],
    similarities: &[Vec<f64>],
    threshold: f64,
) -> Option<(usize, usize, f64)> {
    let mut best: Option<(usize, usize, f64, Vec<usize>)> = None;
    for left in 0..clusters.len() {
        for right in (left + 1)..clusters.len() {
            let similarity =
                average_linkage_similarity(&clusters[left], &clusters[right], similarities);
            if similarity + FLOAT_EPSILON < threshold {
                continue;
            }
            let mut merged = clusters[left].clone();
            merged.extend(clusters[right].iter().copied());
            merged.sort_unstable();
            let replace = match &best {
                None => true,
                Some((best_left, best_right, best_similarity, best_members)) => {
                    similarity
                        .total_cmp(best_similarity)
                        .then_with(|| merged.cmp(best_members).reverse())
                        .then_with(|| (left, right).cmp(&(*best_left, *best_right)).reverse())
                        == Ordering::Greater
                }
            };
            if replace {
                best = Some((left, right, similarity, merged));
            }
        }
    }
    best.map(|(left, right, similarity, _members)| (left, right, similarity))
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
    let nearest_external_similarity = all_clusters
        .iter()
        .filter(|cluster| cluster.as_slice() != members)
        .map(|cluster| average_linkage_similarity(members, cluster, similarities))
        .max_by(f64::total_cmp)
        .map(round_metric);
    let silhouette_score = silhouette_score(
        average_internal_similarity,
        nearest_external_similarity,
        members.len(),
    )
    .map(round_metric);
    let accepted = members.len() >= config.min_cluster_size
        && silhouette_score.is_some_and(|score| score >= config.silhouette_cutoff);
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
    internal_similarity: f64,
    nearest_external_similarity: Option<f64>,
    member_count: usize,
) -> Option<f64> {
    if member_count < 2 {
        return None;
    }
    let Some(nearest_external_similarity) = nearest_external_similarity else {
        return Some(1.0);
    };
    let denominator = internal_similarity
        .abs()
        .max(nearest_external_similarity.abs())
        .max(FLOAT_EPSILON);
    let score = (internal_similarity - nearest_external_similarity) / denominator;
    if score.is_nan() {
        Some(0.0)
    } else {
        Some(score.clamp(-1.0, 1.0))
    }
}

fn cluster_id(member_memory_ids: &[String], threshold: f64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.cluster_coherence.v1\n");
    hash_text(&mut hasher, "threshold", &format!("{threshold:.6}"));
    for memory_id in member_memory_ids {
        hash_text(&mut hasher, "member", memory_id);
    }
    let digest = hasher.finalize().to_hex().to_string();
    format!("clu_{}", &digest[..24])
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
        for value in &mut centroid {
            *value /= members.len() as f64;
        }
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.cluster_centroid.v1\n");
    for value in centroid {
        hash_text(&mut hasher, "value", &format!("{value:.9}"));
    }
    format!("blake3:{}", hasher.finalize().to_hex())
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
