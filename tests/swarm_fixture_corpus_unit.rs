//! S5 deterministic swarm fixture corpus gates.
//!
//! The committed manifest describes large benchmark scales without checking in
//! huge generated corpora. These tests materialize the CI smoke and 10k
//! benchmark shapes in memory, then pin determinism, producer metadata,
//! conflicts, expected pack text, and verification posture.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Value, json};

const MANIFEST_TEXT: &str = include_str!("fixtures/swarm_scale/corpus_manifest.json");
const MANIFEST_SCHEMA_TEXT: &str = include_str!("fixtures/swarm_scale/corpus_manifest.schema.json");
const SMOKE_PACK_GOLDEN: &str =
    include_str!("fixtures/golden/swarm_fixture/smoke_release_pack.md.golden");
const EXPECTED_SCHEMA: &str = "ee.swarm_fixture_corpus.v1";
const EXPECTED_SEED_FAMILY: &str = "seed.swarm_scale.v1";
const PRODUCER_SCHEMA: &str = "ee.producer.metadata.v1";
const HASH_EMBEDDER_DIMENSIONS: usize = 384;

type TestResult = Result<(), String>;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeneratedAgent {
    name: String,
    program: String,
    model: String,
    trust_class: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeneratedMemory {
    id: String,
    topic: String,
    content: String,
    producer: Value,
    trust_class: String,
    verification_status: String,
    embedding_hash: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeneratedCorpus {
    scale: String,
    agents: Vec<GeneratedAgent>,
    memories: Vec<GeneratedMemory>,
    manifest_hash: String,
}

fn manifest() -> Result<Value, String> {
    serde_json::from_str(MANIFEST_TEXT).map_err(|error| format!("manifest JSON failed: {error}"))
}

fn manifest_schema() -> Result<Value, String> {
    serde_json::from_str(MANIFEST_SCHEMA_TEXT)
        .map_err(|error| format!("manifest schema JSON failed: {error}"))
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, String> {
    value
        .get(name)
        .ok_or_else(|| format!("missing field `{name}`"))
}

fn string_field<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    field(value, name)?
        .as_str()
        .ok_or_else(|| format!("field `{name}` must be a string"))
}

fn bool_field(value: &Value, name: &str) -> Result<bool, String> {
    field(value, name)?
        .as_bool()
        .ok_or_else(|| format!("field `{name}` must be a boolean"))
}

fn u64_field(value: &Value, name: &str) -> Result<u64, String> {
    field(value, name)?
        .as_u64()
        .ok_or_else(|| format!("field `{name}` must be an unsigned integer"))
}

fn array_field<'a>(value: &'a Value, name: &str) -> Result<&'a Vec<Value>, String> {
    field(value, name)?
        .as_array()
        .ok_or_else(|| format!("field `{name}` must be an array"))
}

fn object_field<'a>(
    value: &'a Value,
    name: &str,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    field(value, name)?
        .as_object()
        .ok_or_else(|| format!("field `{name}` must be an object"))
}

fn string_array(value: &Value, name: &str) -> Result<Vec<String>, String> {
    array_field(value, name)?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("field `{name}` must contain only strings"))
        })
        .collect()
}

fn ensure(condition: bool, context: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.to_owned())
    }
}

fn ensure_eq<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, context: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn scale<'a>(manifest: &'a Value, name: &str) -> Result<&'a Value, String> {
    array_field(manifest, "scales")?
        .iter()
        .find(|scale| string_field(scale, "name").ok() == Some(name))
        .ok_or_else(|| format!("missing scale `{name}`"))
}

fn memory_id(prefix: &str, ordinal: u64) -> String {
    format!("{prefix}{ordinal:06}")
}

fn generated_agents(manifest: &Value, scale: &Value) -> Result<Vec<GeneratedAgent>, String> {
    let template = field(manifest, "agentTemplate")?;
    let prefix = string_field(template, "namePrefix")?;
    let program_cycle = string_array(template, "programCycle")?;
    let model_cycle = string_array(template, "modelCycle")?;
    let trust_cycle = string_array(template, "trustClassCycle")?;
    let agent_count = usize::try_from(u64_field(scale, "agentCount")?)
        .map_err(|error| format!("agentCount too large: {error}"))?;

    (0..agent_count)
        .map(|index| {
            Ok(GeneratedAgent {
                name: format!("{prefix}{index:02}"),
                program: cycle_value(&program_cycle, index, "programCycle")?,
                model: cycle_value(&model_cycle, index, "modelCycle")?,
                trust_class: cycle_value(&trust_cycle, index, "trustClassCycle")?,
            })
        })
        .collect()
}

fn cycle_value(values: &[String], index: usize, context: &str) -> Result<String, String> {
    if values.is_empty() {
        return Err(format!("{context} must not be empty"));
    }
    Ok(values[index % values.len()].clone())
}

fn generated_corpus(manifest: &Value, scale_name: &str) -> Result<GeneratedCorpus, String> {
    let selected_scale = scale(manifest, scale_name)?;
    let prefix = string_field(selected_scale, "memoryIdPrefix")?;
    let memory_count = usize::try_from(u64_field(selected_scale, "memoryCount")?)
        .map_err(|error| format!("memoryCount too large: {error}"))?;
    let topics = string_array(manifest, "topics")?;
    let agents = generated_agents(manifest, selected_scale)?;
    let fixed_clock = string_field(manifest, "fixedClock")?;
    let manifest_hash = stable_json_hash(manifest)?;

    let memories = (1..=memory_count)
        .map(|index| {
            let ordinal = u64::try_from(index).map_err(|error| error.to_string())?;
            let id = memory_id(prefix, ordinal);
            let topic = cycle_value(&topics, index - 1, "topics")?;
            let agent = agents
                .get((index - 1) % agents.len())
                .ok_or_else(|| "agent cycle unexpectedly empty".to_string())?;
            let verification_status = verification_status_for_memory(&id);
            let content = format!(
                "{topic} memory from {} using {}. Verification status: {verification_status}.",
                agent.name, agent.program
            );
            let embedding = fixture_hash_embedder(&content);
            Ok(GeneratedMemory {
                id,
                topic,
                content,
                producer: json!({
                    "schema": PRODUCER_SCHEMA,
                    "sourceSystem": "verification",
                    "identity": {
                        "status": "known",
                        "agentName": agent.name,
                        "harness": agent.program,
                        "model": agent.model,
                    },
                    "run": {
                        "runId": format!("run-{scale_name}"),
                        "sessionId": format!("session-{scale_name}"),
                        "workspaceFingerprint": "repo:25e38e130474e7f0292de2a3",
                    },
                    "observedAt": fixed_clock,
                }),
                trust_class: agent.trust_class.clone(),
                verification_status,
                embedding_hash: stable_embedding_hash(&embedding),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(GeneratedCorpus {
        scale: scale_name.to_owned(),
        agents,
        memories,
        manifest_hash,
    })
}

fn verification_status_for_memory(id: &str) -> String {
    match id {
        "mem_swarm_smoke_000007" | "mem_swarm_smoke_000008" => "conflict_expected",
        "mem_swarm_smoke_000013" | "mem_swarm_smoke_000014" => "stale_replacement_expected",
        "mem_swarm_smoke_000019" | "mem_swarm_smoke_000020" => "partial_overlap_expected",
        _ => "passed",
    }
    .to_owned()
}

fn stable_json_hash<T: Serialize>(value: &T) -> Result<String, String> {
    let json = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    Ok(format!("blake3:{}", blake3::hash(&json).to_hex()))
}

fn stable_embedding_hash(embedding: &[f32]) -> String {
    let mut hasher = blake3::Hasher::new();
    for value in embedding {
        hasher.update(&value.to_bits().to_le_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn fixture_hash_embedder(content: &str) -> Vec<f32> {
    let mut digest = blake3::Hasher::new();
    digest.update(b"ee.fixture.embedder.v1\0");
    digest.update(content.as_bytes());
    let mut buf = vec![0_u8; HASH_EMBEDDER_DIMENSIONS * 4];
    digest.finalize_xof().fill(&mut buf);
    let mut vector = Vec::with_capacity(HASH_EMBEDDER_DIMENSIONS);
    for chunk in buf.chunks_exact(4) {
        let raw = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let unit = (raw as f32 / u32::MAX as f32) * 2.0 - 1.0;
        vector.push(unit);
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

fn corpus_memory_map(corpus: &GeneratedCorpus) -> BTreeMap<String, &GeneratedMemory> {
    corpus
        .memories
        .iter()
        .map(|memory| (memory.id.clone(), memory))
        .collect()
}

fn expected_pack<'a>(manifest: &'a Value, query: &str) -> Result<&'a Value, String> {
    array_field(manifest, "expectedPacks")?
        .iter()
        .find(|pack| string_field(pack, "query").ok() == Some(query))
        .ok_or_else(|| format!("missing expected pack `{query}`"))
}

fn array_ids(value: &Value, field_name: &str) -> Result<BTreeSet<String>, String> {
    array_field(value, field_name)?
        .iter()
        .map(|item| string_field(item, "id").map(str::to_owned))
        .collect()
}

fn contains_key_recursive(value: &Value, needle: &str) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key(needle)
                || map
                    .values()
                    .any(|item| contains_key_recursive(item, needle))
        }
        Value::Array(items) => items
            .iter()
            .any(|item| contains_key_recursive(item, needle)),
        _ => false,
    }
}

fn validate_manifest_rejects_volatile_keys(manifest: &Value) -> TestResult {
    let denylist = string_array(manifest, "volatileFieldDenylist")?;
    for key in &denylist {
        ensure(
            !contains_key_recursive(manifest, key),
            &format!("manifest must not contain volatile field key `{key}`"),
        )?;
    }
    Ok(())
}

fn render_expected_pack_markdown(
    manifest: &Value,
    corpus: &GeneratedCorpus,
    pack: &Value,
) -> Result<String, String> {
    let memory_by_id = corpus_memory_map(corpus);
    let top_ids = string_array(pack, "expectedTop3Ids")?;
    let mut output = String::new();
    output.push_str("# Swarm Fixture Pack\n\n");
    output.push_str(&format!("Query: {}\n", string_field(pack, "query")?));
    output.push_str(&format!("Scale: {}\n", string_field(pack, "scale")?));
    output.push_str(&format!(
        "Manifest: {}\n\n",
        string_field(manifest, "corpusId")?
    ));
    for (index, memory_id) in top_ids.iter().enumerate() {
        let memory = memory_by_id
            .get(memory_id)
            .ok_or_else(|| format!("expected pack references missing memory {memory_id}"))?;
        output.push_str(&format!(
            "{}. {} - {}\n",
            index + 1,
            memory.id,
            memory.content
        ));
    }
    Ok(output.trim_end().to_owned())
}

#[test]
fn swarm_fixture_manifest_contract_is_pinned() -> TestResult {
    let schema = manifest_schema()?;
    let manifest = manifest()?;
    ensure_eq(
        string_field(&schema, "title")?,
        "ee.swarm_fixture_corpus.v1 test manifest",
        "schema title",
    )?;
    ensure_eq(
        string_field(&manifest, "schema")?,
        EXPECTED_SCHEMA,
        "manifest schema",
    )?;
    ensure_eq(
        string_field(&manifest, "seedFamily")?,
        EXPECTED_SEED_FAMILY,
        "seed family",
    )?;
    ensure_eq(
        u64_field(field(&manifest, "hashEmbedder")?, "dimensions")?,
        u64::try_from(HASH_EMBEDDER_DIMENSIONS).map_err(|error| error.to_string())?,
        "hash embedder dimensions",
    )?;

    let micro = scale(&manifest, "micro_256")?;
    ensure_eq(u64_field(micro, "agentCount")?, 16, "micro agents")?;
    ensure_eq(u64_field(micro, "memoryCount")?, 256, "micro memories")?;
    ensure_eq(
        u64_field(micro, "expectedGraphNodes")?,
        272,
        "micro graph nodes",
    )?;
    ensure_eq(
        u64_field(micro, "expectedGraphEdges")?,
        640,
        "micro graph edges",
    )?;
    ensure(
        bool_field(micro, "materializedInCi")?,
        "micro corpus must be CI-materialized",
    )?;

    let smoke = scale(&manifest, "smoke_1k")?;
    ensure_eq(u64_field(smoke, "agentCount")?, 64, "smoke agents")?;
    ensure_eq(u64_field(smoke, "memoryCount")?, 1_000, "smoke memories")?;
    ensure_eq(
        u64_field(smoke, "expectedGraphNodes")?,
        1_064,
        "smoke graph nodes",
    )?;
    ensure_eq(
        u64_field(smoke, "expectedGraphEdges")?,
        2_500,
        "smoke graph edges",
    )?;
    ensure(
        bool_field(smoke, "materializedInCi")?,
        "smoke corpus must be CI-materialized",
    )?;

    let mid = scale(&manifest, "mid_10k")?;
    ensure(
        u64_field(mid, "agentCount")? >= 64,
        "benchmark corpus must model at least 64 agents",
    )?;
    ensure(
        u64_field(mid, "memoryCount")? >= 10_000,
        "benchmark corpus must model at least 10k memories",
    )?;

    let large = scale(&manifest, "large_100k")?;
    ensure_eq(u64_field(large, "agentCount")?, 256, "large agents")?;
    ensure(
        u64_field(large, "memoryCount")? >= 100_000,
        "large corpus must model 100k memories",
    )
}

#[test]
fn generated_fixture_corpora_cover_16_64_and_256_agent_scales() -> TestResult {
    let manifest = manifest()?;
    let micro = generated_corpus(&manifest, "micro_256")?;
    let mid = generated_corpus(&manifest, "mid_10k")?;
    let large_scale = scale(&manifest, "large_100k")?;

    ensure_eq(micro.agents.len(), 16, "micro agents")?;
    ensure_eq(micro.memories.len(), 256, "micro memories")?;
    ensure_eq(
        micro.memories.first().map(|memory| memory.id.as_str()),
        Some("mem_swarm_micro_000001"),
        "micro first id",
    )?;
    ensure_eq(
        micro.memories.last().map(|memory| memory.id.as_str()),
        Some("mem_swarm_micro_000256"),
        "micro last id",
    )?;
    ensure_eq(mid.agents.len(), 64, "mid agents")?;
    ensure_eq(u64_field(large_scale, "agentCount")?, 256, "large agents")?;
    Ok(())
}

#[test]
fn generated_smoke_corpus_is_deterministic_and_provenanced() -> TestResult {
    let manifest = manifest()?;
    let first = generated_corpus(&manifest, "smoke_1k")?;
    let second = generated_corpus(&manifest, "smoke_1k")?;
    ensure_eq(
        stable_json_hash(&first)?,
        stable_json_hash(&second)?,
        "smoke corpus hash",
    )?;
    ensure(first == second, "smoke corpus structure")?;
    ensure_eq(first.agents.len(), 64, "agent count")?;
    ensure_eq(first.memories.len(), 1_000, "memory count")?;

    let first_memory = first
        .memories
        .first()
        .ok_or_else(|| "smoke corpus missing first memory".to_string())?;
    ensure_eq(
        first_memory.id.as_str(),
        "mem_swarm_smoke_000001",
        "first memory id",
    )?;
    ensure_eq(
        first_memory.producer.get("schema").and_then(Value::as_str),
        Some(PRODUCER_SCHEMA),
        "producer schema",
    )?;
    ensure_eq(
        first_memory
            .producer
            .pointer("/identity/status")
            .and_then(Value::as_str),
        Some("known"),
        "producer identity status",
    )?;
    ensure_eq(
        first_memory.verification_status.as_str(),
        "passed",
        "verification status",
    )
}

#[test]
fn generated_benchmark_corpus_covers_64_agents_and_10k_memories() -> TestResult {
    let manifest = manifest()?;
    let corpus = generated_corpus(&manifest, "mid_10k")?;
    ensure_eq(corpus.agents.len(), 64, "mid_10k agents")?;
    ensure_eq(corpus.memories.len(), 10_000, "mid_10k memories")?;
    ensure_eq(
        corpus.memories.first().map(|memory| memory.id.as_str()),
        Some("mem_swarm_mid_000001"),
        "mid first id",
    )?;
    ensure_eq(
        corpus.memories.last().map(|memory| memory.id.as_str()),
        Some("mem_swarm_mid_010000"),
        "mid last id",
    )?;
    let trust_classes = corpus
        .agents
        .iter()
        .map(|agent| agent.trust_class.clone())
        .collect::<BTreeSet<_>>();
    ensure(
        trust_classes.len() >= 4,
        "benchmark corpus must include diverse trust classes",
    )
}

#[test]
fn conflicts_and_expected_packs_reference_real_memories() -> TestResult {
    let manifest = manifest()?;
    let corpus = generated_corpus(&manifest, "smoke_1k")?;
    let memory_ids = corpus
        .memories
        .iter()
        .map(|memory| memory.id.clone())
        .collect::<BTreeSet<_>>();

    let mut conflict_kinds = BTreeSet::new();
    for conflict in array_field(&manifest, "conflicts")? {
        conflict_kinds.insert(string_field(conflict, "kind")?.to_owned());
        for memory_id in string_array(conflict, "memoryIds")? {
            ensure(
                memory_ids.contains(&memory_id),
                &format!("conflict references missing memory {memory_id}"),
            )?;
        }
    }
    ensure_eq(
        conflict_kinds,
        ["direct", "partial_overlap", "stale_replacement"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>(),
        "conflict kinds",
    )?;

    for pack in array_field(&manifest, "expectedPacks")? {
        for memory_id in string_array(pack, "expectedTop3Ids")? {
            ensure(
                memory_ids.contains(&memory_id),
                &format!("expected pack references missing memory {memory_id}"),
            )?;
        }
    }
    Ok(())
}

#[test]
fn scenario_state_sections_cover_degraded_stale_and_contention_inputs() -> TestResult {
    let manifest = manifest()?;
    let states = field(&manifest, "scenarioStates")?;
    let state_object = object_field(&manifest, "scenarioStates")?;
    ensure_eq(state_object.len(), 5, "scenario state section count")?;

    ensure(
        array_ids(states, "dirtyWorktrees")?.contains("overlap_swarm_core"),
        "dirty worktree states must include overlap risk",
    )?;
    ensure(
        array_ids(states, "coordinationSources")?.contains("degraded_stale_mix"),
        "coordination sources must include degraded/stale mix",
    )?;
    ensure(
        array_ids(states, "beadsGraphs")?.contains("blocked_rch_gate"),
        "Beads graph states must include RCH-blocked gate",
    )?;
    ensure(
        array_ids(states, "agentMailSnapshots")?.contains("unreachable"),
        "Agent Mail snapshots must include unreachable state",
    )?;
    ensure(
        array_ids(states, "rchPressureSnapshots")?.contains("topology_blocked"),
        "RCH pressure snapshots must include topology-blocked state",
    )
}

#[test]
fn fixture_manifest_rejects_volatile_field_names_by_contract() -> TestResult {
    let manifest = manifest()?;
    let denylist = string_array(&manifest, "volatileFieldDenylist")?;
    ensure(
        denylist.contains(&"generatedAt".to_string())
            && denylist.contains(&"timestamp".to_string())
            && denylist.contains(&"wallClockMs".to_string()),
        "volatile field denylist must cover wall-clock and generated timestamp fields",
    )?;
    validate_manifest_rejects_volatile_keys(&manifest)
}

#[test]
fn fixture_manifest_validation_rejects_nondeterministic_fields() -> TestResult {
    let mut manifest = manifest()?;
    manifest["scenarioStates"]["rchPressureSnapshots"][0]["generatedAt"] =
        json!("2026-05-14T07:24:00Z");
    let error = match validate_manifest_rejects_volatile_keys(&manifest) {
        Ok(()) => {
            return Err("volatile generatedAt field should be rejected".to_owned());
        }
        Err(error) => error,
    };
    ensure(
        error.contains("generatedAt"),
        "volatile field rejection should identify generatedAt",
    )
}

#[test]
fn scenario_state_summary_hash_is_byte_identical() -> TestResult {
    let manifest = manifest()?;
    let summary = json!({
        "schema": EXPECTED_SCHEMA,
        "scales": field(&manifest, "scales")?,
        "scenarioStates": field(&manifest, "scenarioStates")?,
    });
    let first = stable_json_hash(&summary)?;
    let second = stable_json_hash(&summary)?;
    ensure_eq(first, second, "scenario state summary hash")?;
    ensure(
        stable_json_hash(&summary)?.starts_with("blake3:"),
        "summary hash must use blake3 prefix",
    )
}

#[test]
fn expected_pack_markdown_golden_is_stable() -> TestResult {
    let manifest = manifest()?;
    let corpus = generated_corpus(&manifest, "smoke_1k")?;
    let pack = expected_pack(&manifest, "release verification coordination")?;
    let rendered = render_expected_pack_markdown(&manifest, &corpus, pack)?;
    ensure_eq(
        rendered.as_str(),
        SMOKE_PACK_GOLDEN.trim_end(),
        "smoke release pack golden",
    )?;
    ensure_eq(
        string_field(pack, "markdownGolden")?,
        "tests/fixtures/golden/swarm_fixture/smoke_release_pack.md.golden",
        "golden path",
    )
}

#[test]
fn fixture_hash_embedder_is_unit_length_and_deterministic() -> TestResult {
    let left = fixture_hash_embedder("release verification coordination");
    let right = fixture_hash_embedder("release verification coordination");
    let other = fixture_hash_embedder("database migration handoff");
    ensure_eq(left.len(), HASH_EMBEDDER_DIMENSIONS, "embedding dimension")?;
    ensure(left == right, "embedding determinism")?;
    ensure(
        left != other,
        "different text should produce different embeddings",
    )?;
    let norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    ensure(
        (norm - 1.0).abs() < 0.000_01,
        "embedding should be normalized",
    )
}
