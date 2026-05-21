//! Roaring-encoded graph algorithm result-cache parameter keys.

use roaring::RoaringBitmap;

use crate::graph::{GRAPH_ALGORITHM_RESULT_CACHE_KEY_SCHEMA_V1, GraphError, GraphResult};

pub const GRAPH_ALGORITHM_RESULT_CACHE_KEY_SCHEMA_ROARING_V1: &str =
    "ee.graph.algorithm_result_cache_key.roaring_params.v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphAlgorithmParamBitmap {
    bitmap: RoaringBitmap,
    token_count: usize,
    token_digest: String,
}

impl GraphAlgorithmParamBitmap {
    #[must_use]
    pub fn bit_count(&self) -> u64 {
        self.bitmap.len()
    }

    #[must_use]
    pub fn token_count(&self) -> usize {
        self.token_count
    }

    #[must_use]
    pub fn token_digest(&self) -> &str {
        self.token_digest.as_str()
    }

    #[must_use]
    pub fn serialized_len(&self) -> usize {
        self.bitmap.serialized_size()
    }

    pub fn serialized_bytes(&self) -> GraphResult<Vec<u8>> {
        let mut bytes = Vec::with_capacity(self.bitmap.serialized_size());
        self.bitmap
            .serialize_into(&mut bytes)
            .map_err(|error| GraphError::GraphEngine {
                operation: "serialize graph algorithm param bitmap",
                source: error.to_string(),
            })?;
        Ok(bytes)
    }

    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        !self.bitmap.is_disjoint(&other.bitmap)
    }

    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        let mut bitmap = self.bitmap.clone();
        for bit in other.bitmap.iter() {
            bitmap.insert(bit);
        }
        Self::from_bitmap(bitmap)
    }

    #[must_use]
    pub fn difference(&self, other: &Self) -> Self {
        let mut bitmap = RoaringBitmap::new();
        for bit in self.bitmap.iter() {
            if !other.bitmap.contains(bit) {
                bitmap.insert(bit);
            }
        }
        Self::from_bitmap(bitmap)
    }

    fn from_bitmap(bitmap: RoaringBitmap) -> Self {
        let mut hasher = blake3::Hasher::new();
        let mut count = 0usize;
        for bit in bitmap.iter() {
            count = count.saturating_add(1);
            hasher.update(&bit.to_le_bytes());
        }
        Self {
            bitmap,
            token_count: count,
            token_digest: format!("blake3:{}", hasher.finalize().to_hex()),
        }
    }
}

pub fn graph_algorithm_params_hash(
    algorithm: &str,
    snapshot_content_hash: &str,
    params: &serde_json::Value,
) -> GraphResult<String> {
    let encoded = encode_graph_algorithm_params_bitmap(params)?;
    let encoded_bytes = encoded.serialized_bytes()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(GRAPH_ALGORITHM_RESULT_CACHE_KEY_SCHEMA_ROARING_V1.as_bytes());
    hasher.update(b"\0");
    hasher.update(algorithm.as_bytes());
    hasher.update(b"\0");
    hasher.update(snapshot_content_hash.as_bytes());
    hasher.update(b"\0");
    hasher.update(encoded.token_digest().as_bytes());
    hasher.update(b"\0");
    hasher.update(&usize_to_u64_saturating(encoded.token_count()).to_le_bytes());
    hasher.update(b"\0");
    hasher.update(&encoded_bytes);
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

pub fn graph_algorithm_legacy_json_params_hash(
    algorithm: &str,
    snapshot_content_hash: &str,
    params: &serde_json::Value,
) -> GraphResult<String> {
    let canonical_params = canonical_graph_algorithm_params_json(params)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(GRAPH_ALGORITHM_RESULT_CACHE_KEY_SCHEMA_V1.as_bytes());
    hasher.update(b"\0");
    hasher.update(algorithm.as_bytes());
    hasher.update(b"\0");
    hasher.update(snapshot_content_hash.as_bytes());
    hasher.update(b"\0");
    hasher.update(canonical_params.as_bytes());
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

pub fn encode_graph_algorithm_params_bitmap(
    params: &serde_json::Value,
) -> GraphResult<GraphAlgorithmParamBitmap> {
    let mut tokens = Vec::new();
    collect_param_tokens("$", params, &mut tokens)?;
    tokens.sort();
    tokens.dedup();

    let mut bitmap = RoaringBitmap::new();
    let mut digest = blake3::Hasher::new();
    for token in &tokens {
        digest.update(token.as_bytes());
        digest.update(b"\0");
        bitmap.insert(param_token_bit(token));
    }

    Ok(GraphAlgorithmParamBitmap {
        bitmap,
        token_count: tokens.len(),
        token_digest: format!("blake3:{}", digest.finalize().to_hex()),
    })
}

pub fn canonical_graph_algorithm_params_json(params: &serde_json::Value) -> GraphResult<String> {
    serde_json::to_string(&canonical_graph_algorithm_params_value(params))
        .map_err(|error| GraphError::json("serialize graph algorithm cache params", error))
}

fn canonical_graph_algorithm_params_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(canonical_graph_algorithm_params_value)
                .collect(),
        ),
        serde_json::Value::Object(fields) => {
            let mut sorted = serde_json::Map::new();
            let mut keys: Vec<_> = fields.keys().collect();
            keys.sort();
            for key in keys {
                if let Some(value) = fields.get(key) {
                    sorted.insert(key.clone(), canonical_graph_algorithm_params_value(value));
                }
            }
            serde_json::Value::Object(sorted)
        }
        other => other.clone(),
    }
}

fn collect_param_tokens(
    path: &str,
    value: &serde_json::Value,
    tokens: &mut Vec<String>,
) -> GraphResult<()> {
    match value {
        serde_json::Value::Null => tokens.push(format!("{path}=null")),
        serde_json::Value::Bool(value) => tokens.push(format!("{path}={value}")),
        serde_json::Value::Number(value) => tokens.push(format!("{path}={value}")),
        serde_json::Value::String(value) => {
            let encoded = serde_json::to_string(value)
                .map_err(|error| GraphError::json("serialize graph cache string param", error))?;
            tokens.push(format!("{path}={encoded}"));
        }
        serde_json::Value::Array(items) => {
            tokens.push(format!("{path}.len={}", items.len()));
            if items.is_empty() {
                tokens.push(format!("{path}=[]"));
            }
            for (index, item) in items.iter().enumerate() {
                collect_param_tokens(&format!("{path}[{index}]"), item, tokens)?;
            }
        }
        serde_json::Value::Object(fields) => {
            if fields.is_empty() {
                tokens.push(format!("{path}={{}}"));
            }
            let mut keys: Vec<_> = fields.keys().collect();
            keys.sort();
            for key in keys {
                let encoded_key = serde_json::to_string(key).map_err(|error| {
                    GraphError::json("serialize graph cache object key param", error)
                })?;
                if let Some(value) = fields.get(key) {
                    collect_param_tokens(&format!("{path}.{encoded_key}"), value, tokens)?;
                }
            }
        }
    }
    Ok(())
}

fn param_token_bit(token: &str) -> u32 {
    let digest = blake3::hash(token.as_bytes());
    let bytes = digest.as_bytes();
    // Keep the bitmap positions clustered so Roaring gets real compression.
    // The token digest above remains part of the final cache hash, so this
    // compact bitmap is an intersection sketch rather than the sole identity.
    u32::from(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn usize_to_u64_saturating(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn graph_result<T>(result: GraphResult<T>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }

    #[test]
    fn roaring_params_hash_preserves_existing_canonical_order_contract() -> TestResult {
        let first = serde_json::json!({
            "damping": 0.85,
            "seeds": ["mem_b", "mem_a"],
            "weights": {
                "beta": 2,
                "alpha": 1
            }
        });
        let second = serde_json::json!({
            "weights": {
                "alpha": 1,
                "beta": 2
            },
            "seeds": ["mem_b", "mem_a"],
            "damping": 0.85
        });

        let first_hash = graph_result(graph_algorithm_params_hash(
            "pagerank",
            "blake3:snapshot-a",
            &first,
        ))?;
        let second_hash = graph_result(graph_algorithm_params_hash(
            "pagerank",
            "blake3:snapshot-a",
            &second,
        ))?;
        let legacy_hash = graph_result(graph_algorithm_legacy_json_params_hash(
            "pagerank",
            "blake3:snapshot-a",
            &second,
        ))?;

        assert_eq!(first_hash, second_hash);
        assert_ne!(first_hash, legacy_hash);
        assert!(first_hash.starts_with("blake3:"));
        Ok(())
    }

    #[test]
    fn roaring_param_sets_support_intersection_union_and_difference() -> TestResult {
        let base = graph_result(encode_graph_algorithm_params_bitmap(&serde_json::json!({
            "algorithm": "pagerank",
            "damping": 0.85,
            "seed": "mem_a"
        })))?;
        let overlapping = graph_result(encode_graph_algorithm_params_bitmap(&serde_json::json!({
            "algorithm": "pagerank",
            "damping": 0.85,
            "seed": "mem_b"
        })))?;
        let disjoint = graph_result(encode_graph_algorithm_params_bitmap(&serde_json::json!({
            "algorithm": "hits",
            "authority": true
        })))?;

        assert!(base.intersects(&overlapping));
        assert!(!base.intersects(&disjoint));

        let union = base.union(&disjoint);
        assert!(union.bit_count() >= base.bit_count());
        assert!(union.bit_count() >= disjoint.bit_count());

        let difference = union.difference(&disjoint);
        assert_eq!(
            difference.bit_count(),
            base.difference(&disjoint).bit_count()
        );
        assert!(!difference.intersects(&disjoint));
        Ok(())
    }

    #[test]
    fn roaring_params_encoding_is_smaller_than_canonical_json_for_sparse_params() -> TestResult {
        let params = serde_json::json!({
            "algorithm": "personalized_pagerank",
            "damping": 0.85,
            "maxIterations": 50,
            "tolerance": 0.000001,
            "seedMemories": [
                "mem_00000000000000000000000001",
                "mem_00000000000000000000000002",
                "mem_00000000000000000000000003",
                "mem_00000000000000000000000004"
            ],
            "weights": {
                "recency": 0.25,
                "trust": 0.35,
                "structural": 0.40
            },
            "queryProfile": "agent-context-refresh-with-graph-structural-reranking",
            "workspaceScope": "local-first-derived-graph-cache-for-maintenance-jobs",
            "decisionPath": "seeded-ppr-before-pack-dna-before-hits-authority-profile"
        });
        let json_len = graph_result(canonical_graph_algorithm_params_json(&params))?.len();
        let roaring_len =
            graph_result(encode_graph_algorithm_params_bitmap(&params))?.serialized_len();

        assert!(
            json_len >= roaring_len.saturating_mul(3),
            "expected >=3x smaller roaring params; json={json_len} roaring={roaring_len}"
        );
        Ok(())
    }
}
