#![forbid(unsafe_code)]

use ee::core::curate::{ClusterCoherenceInput, silhouette_agglomerative_clusters};
use ee::curate::cluster_coherence::{
    ClusterCoherenceConfig, EmbeddingPoint, agglomerate as canonical_agglomerate,
};

type TestResult = Result<(), String>;

fn unit_vector(angle_radians: f32) -> Vec<f32> {
    vec![angle_radians.cos(), angle_radians.sin()]
}

fn synthetic_embeddings() -> Vec<ClusterCoherenceInput> {
    let mut inputs = Vec::new();
    for (index, angle) in [
        -0.20_f32, -0.16, -0.12, -0.08, -0.04, 0.0, 0.04, 0.08, 0.12, 0.16, 0.20,
    ]
    .into_iter()
    .enumerate()
    {
        inputs.push(ClusterCoherenceInput {
            memory_id: format!("mem_cargo_{index:02}"),
            embedding: unit_vector(angle),
        });
    }
    let base = 0.3_f32.acos();
    for (index, offset) in [-0.15_f32, 0.0, 0.15].into_iter().enumerate() {
        inputs.push(ClusterCoherenceInput {
            memory_id: format!("mem_db_{index:02}"),
            embedding: unit_vector(base + offset),
        });
    }
    inputs
}

fn cluster_sizes(report: &ee::core::curate::ClusterCoherenceReport) -> Vec<usize> {
    let mut sizes = report
        .clusters
        .iter()
        .map(|cluster| cluster.member_memory_ids.len())
        .collect::<Vec<_>>();
    sizes.sort_unstable();
    sizes
}

#[test]
fn agglomerative_clustering_finds_two_stable_clusters() -> TestResult {
    let inputs = synthetic_embeddings();
    let first = silhouette_agglomerative_clusters(&inputs, 0.80);
    let second = silhouette_agglomerative_clusters(&inputs, 0.80);
    let third = silhouette_agglomerative_clusters(&inputs, 0.80);

    if cluster_sizes(&first) != vec![3, 11] {
        return Err(format!(
            "expected cargo/db clusters sized 11 and 3, got {:?}",
            cluster_sizes(&first)
        ));
    }
    if first.clusters != second.clusters || second.clusters != third.clusters {
        return Err("cluster output should be byte-stable across repeated runs".to_owned());
    }
    for cluster in &first.clusters {
        let silhouette = cluster
            .silhouette_score
            .ok_or_else(|| format!("cluster {} missing silhouette", cluster.cluster_id))?;
        if silhouette < 0.50 {
            return Err(format!(
                "cluster {} silhouette too low: {silhouette}",
                cluster.cluster_id
            ));
        }
    }
    Ok(())
}

#[test]
fn threshold_sweep_documents_over_merge_and_over_split() -> TestResult {
    let inputs = synthetic_embeddings();
    let loose = silhouette_agglomerative_clusters(&inputs, 0.20);
    if cluster_sizes(&loose) != vec![14] {
        return Err(format!(
            "loose threshold should over-merge into one cluster, got {:?}",
            cluster_sizes(&loose)
        ));
    }

    let strict = silhouette_agglomerative_clusters(&inputs, 0.995);
    if strict.clusters.len() <= 2 {
        return Err(format!(
            "strict threshold should over-split, got {} clusters",
            strict.clusters.len()
        ));
    }
    Ok(())
}

#[test]
fn learn_cluster_adapter_uses_canonical_cluster_ids() -> TestResult {
    let inputs = synthetic_embeddings();
    let adapter = silhouette_agglomerative_clusters(&inputs, 0.80);
    let canonical_points = inputs
        .iter()
        .map(|input| {
            EmbeddingPoint::new(
                input.memory_id.clone(),
                input
                    .embedding
                    .iter()
                    .map(|value| f64::from(*value))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let canonical = canonical_agglomerate(
        &canonical_points,
        ClusterCoherenceConfig {
            merge_threshold: 0.80,
            ..ClusterCoherenceConfig::default()
        },
    )
    .map_err(|error| error.to_string())?;
    let adapter_ids = adapter
        .clusters
        .iter()
        .map(|cluster| cluster.cluster_id.as_str())
        .collect::<Vec<_>>();
    let canonical_ids = canonical
        .clusters
        .iter()
        .map(|cluster| cluster.cluster_id.as_str())
        .collect::<Vec<_>>();
    if adapter_ids != canonical_ids {
        return Err(format!(
            "learn cluster adapter diverged from canonical IDs: {adapter_ids:?} vs {canonical_ids:?}"
        ));
    }
    Ok(())
}

#[test]
fn empty_and_singleton_inputs_degrade_honestly() -> TestResult {
    let empty = silhouette_agglomerative_clusters(&[], 0.80);
    if empty.degradations != vec!["degraded.clustering_insufficient_data"] {
        return Err(format!(
            "empty input degradation mismatch: {:?}",
            empty.degradations
        ));
    }

    let singleton = silhouette_agglomerative_clusters(
        &[ClusterCoherenceInput {
            memory_id: "mem_singleton".to_owned(),
            embedding: unit_vector(0.0),
        }],
        0.80,
    );
    if singleton.clusters.len() != 1 {
        return Err(format!(
            "singleton should return one singleton cluster, got {}",
            singleton.clusters.len()
        ));
    }
    if singleton.clusters[0].silhouette_score.is_some()
        || singleton.clusters[0].degradations
            != vec!["degraded.clustering_silhouette_undefined_for_singleton"]
    {
        return Err(format!(
            "singleton silhouette degradation mismatch: {:?}",
            singleton.clusters[0]
        ));
    }
    Ok(())
}
