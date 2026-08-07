//! bd-95qe4 — CI-runnable, model-free regression for the reranker
//! nested-rayon + `Mutex` deadlock class.
//!
//! ## The production deadlock scenario being preserved
//!
//! The native cross-encoder reranker historically hung when doc-level rayon
//! dispatch (`par_iter`) held a session `Mutex` while the forward pass fanned
//! work onto the same ambient rayon pool: a worker waiting inside a nested
//! `join` could steal a sibling document task that blocked on the very
//! `Mutex` the waiter already held. The shipped fix removed doc-level
//! parallelism: `NativeReranker::rerank_sync` locks one session `Mutex` once
//! and reranks documents in a sequential, token-budgeted chunk loop while each
//! forward parallelizes internally (int8 linear GEMMs, attention `bmm`,
//! parallel softmax) on ambient rayon. If a future change reintroduces
//! doc-level rayon dispatch under the session lock, these tests wedge instead
//! of silently shipping the hang — the watchdog converts the wedge into a
//! failure with the deadlock-class diagnosis.
//!
//! ## Why this is model-free but NOT a mock
//!
//! The pinned upstream regression test
//! (`frankensearch/crates/frankensearch-rerank/src/native.rs`,
//! `many_documents_rerank_without_deadlock`) exercises the real scenario but
//! silently skips unless the real 83 MiB `ms-marco-MiniLM-L6-v2` artifact is
//! present at a hardcoded path, so a model-free CI lane proves nothing. These
//! tests generate a tiny, architecturally exact checkpoint at runtime — the
//! full `BertForSequenceClassification` tensor set with the real shapes
//! (hidden 384, 6 layers, FFN 1536) plus a real word-level `tokenizer.json` —
//! and drive the REAL production code end to end: `NativeReranker::load`
//! (real safetensors parse, real per-output-channel int8 quantization, real
//! tokenizer load) and `rerank_sync` (real session `Mutex`, real chunked
//! batched forward, real frankentorch kernels on ambient rayon). Only the
//! weight VALUES and the vocabulary are synthetic; the deadlock class lives
//! entirely in the locking/threading structure, which is independent of what
//! the weights contain. Scores are asserted for shape, finiteness, order
//! preservation, and bit-exact determinism — the contracts `rerank_sync`
//! documents — never for semantic ranking quality, which stays owned by the
//! model-gated upstream test and the `EE_E2E_RERANK_MODEL_DIR` determinism
//! lane in `tests/determinism_unit.rs`.
//!
//! ## Production shapes covered
//!
//! * Many documents (24 ≫ the historical 8-slot session-pool cap) through one
//!   `rerank_sync` call whose total token count exceeds the batched forward's
//!   per-chunk token budget, so the sequential chunk loop crosses a chunk
//!   boundary while the session `Mutex` is held — the exact multi-doc path
//!   that previously hung.
//! * Concurrent callers sharing one reranker `Arc`, matching how ee holds the
//!   loaded reranker as `Arc<dyn Reranker>` (`SyncRerankerAdapter` in
//!   `src/core/search.rs`) where multiple in-process searches contend on the
//!   session `Mutex` while each locked forward fans onto ambient rayon.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use frankensearch::{NativeReranker, RerankDocument, RerankScore, SyncRerank};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

/// Hidden size of the ms-marco-MiniLM-L6-v2 architecture the native reranker
/// hardcodes; every generated tensor must use these exact dimensions or
/// `NativeReranker` rejects the checkpoint.
const HIDDEN: usize = 384;
const LAYERS: usize = 6;
const INTERMEDIATE: usize = 1536;
const POSITIONS: usize = 512;
const TOKEN_TYPES: usize = 2;

/// Mirror of frankensearch's private `MAX_BATCH_TOKENS` (the per-chunk token
/// budget of the batched forward). The many-doc test sizes its corpus above
/// this so the sequential chunk loop demonstrably crosses a chunk boundary
/// under the held session `Mutex`. If the pinned frankensearch revision
/// changes the budget, this mirror only weakens toward "fewer chunks", never
/// toward a false failure.
const CHUNK_TOKEN_BUDGET_MIRROR: usize = 2048;

/// Word-level vocabulary. Every fixture document and the query are composed
/// from these words, so each whitespace-split word maps to exactly one token
/// id and per-document token counts are exact, not estimates.
const VOCAB_WORDS: [&str; 40] = [
    "release",
    "format",
    "checklist",
    "cargo",
    "clippy",
    "rust",
    "memory",
    "search",
    "index",
    "pack",
    "policy",
    "deadlock",
    "rayon",
    "mutex",
    "forward",
    "batch",
    "chunk",
    "tokenize",
    "session",
    "lock",
    "kernel",
    "tensor",
    "linear",
    "softmax",
    "attention",
    "pooler",
    "classifier",
    "embedding",
    "layer",
    "norm",
    "quantize",
    "score",
    "logit",
    "rank",
    "order",
    "input",
    "output",
    "budget",
    "token",
    "doc",
];

/// Five vocabulary words + [CLS] + 2×[SEP] template tokens.
const QUERY: &str = "release format checklist cargo clippy";
const QUERY_WORDS: usize = 5;
const TEMPLATE_SPECIAL_TOKENS: usize = 3;

struct TensorSpec {
    name: String,
    shape: Vec<usize>,
    values: Vec<f32>,
}

/// Deterministic per-tensor pseudo-random values (FNV-1a name seed +
/// xorshift64), bounded by `scale`. Deterministic weights keep every rerank
/// run bit-identical, which the determinism assertions rely on.
fn seeded_values(name: &str, len: usize, scale: f32) -> Vec<f32> {
    let mut state: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.bytes() {
        state ^= u64::from(byte);
        state = state.wrapping_mul(0x0000_0100_0000_01b3);
    }
    if state == 0 {
        state = 0x9e37_79b9_7f4a_7c15;
    }
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let byte = u8::try_from((state >> 24) & 0xff).expect("masked to one byte");
        out.push((f32::from(byte) - 127.5) * (scale / 127.5));
    }
    out
}

fn push_random(tensors: &mut Vec<TensorSpec>, name: &str, shape: &[usize], scale: f32) {
    let len = shape.iter().product();
    tensors.push(TensorSpec {
        name: name.to_owned(),
        shape: shape.to_vec(),
        values: seeded_values(name, len, scale),
    });
}

/// A Linear block: `.weight` (int8-quantized by the real loader) + `.bias`.
fn push_linear(tensors: &mut Vec<TensorSpec>, prefix: &str, out_dim: usize, in_dim: usize) {
    push_random(
        tensors,
        &format!("{prefix}.weight"),
        &[out_dim, in_dim],
        0.05,
    );
    push_random(tensors, &format!("{prefix}.bias"), &[out_dim], 0.01);
}

/// A LayerNorm block: identity gain, zero bias (kept f32 by the real loader).
fn push_layer_norm(tensors: &mut Vec<TensorSpec>, prefix: &str) {
    tensors.push(TensorSpec {
        name: format!("{prefix}.weight"),
        shape: vec![HIDDEN],
        values: vec![1.0; HIDDEN],
    });
    tensors.push(TensorSpec {
        name: format!("{prefix}.bias"),
        shape: vec![HIDDEN],
        values: vec![0.0; HIDDEN],
    });
}

/// The complete `BertForSequenceClassification` tensor set the native
/// reranker's forward looks up, at the exact hardcoded architecture shapes.
fn fixture_tensors() -> Vec<TensorSpec> {
    let vocab_size = VOCAB_WORDS.len() + 4; // [PAD]/[UNK]/[CLS]/[SEP]
    let mut tensors = Vec::new();
    push_random(
        &mut tensors,
        "bert.embeddings.word_embeddings.weight",
        &[vocab_size, HIDDEN],
        0.05,
    );
    push_random(
        &mut tensors,
        "bert.embeddings.position_embeddings.weight",
        &[POSITIONS, HIDDEN],
        0.02,
    );
    push_random(
        &mut tensors,
        "bert.embeddings.token_type_embeddings.weight",
        &[TOKEN_TYPES, HIDDEN],
        0.02,
    );
    push_layer_norm(&mut tensors, "bert.embeddings.LayerNorm");
    for layer in 0..LAYERS {
        let p = format!("bert.encoder.layer.{layer}");
        push_linear(
            &mut tensors,
            &format!("{p}.attention.self.query"),
            HIDDEN,
            HIDDEN,
        );
        push_linear(
            &mut tensors,
            &format!("{p}.attention.self.key"),
            HIDDEN,
            HIDDEN,
        );
        push_linear(
            &mut tensors,
            &format!("{p}.attention.self.value"),
            HIDDEN,
            HIDDEN,
        );
        push_linear(
            &mut tensors,
            &format!("{p}.attention.output.dense"),
            HIDDEN,
            HIDDEN,
        );
        push_layer_norm(&mut tensors, &format!("{p}.attention.output.LayerNorm"));
        push_linear(
            &mut tensors,
            &format!("{p}.intermediate.dense"),
            INTERMEDIATE,
            HIDDEN,
        );
        push_linear(
            &mut tensors,
            &format!("{p}.output.dense"),
            HIDDEN,
            INTERMEDIATE,
        );
        push_layer_norm(&mut tensors, &format!("{p}.output.LayerNorm"));
    }
    push_linear(&mut tensors, "bert.pooler.dense", HIDDEN, HIDDEN);
    push_linear(&mut tensors, "classifier", 1, HIDDEN);
    tensors
}

/// Serialize tensors in the safetensors container format (8-byte LE header
/// length, JSON header with `data_offsets`, then raw little-endian f32 data).
fn write_safetensors(path: &Path, tensors: &[TensorSpec]) -> TestResult {
    let mut header = serde_json::Map::new();
    let mut offset = 0usize;
    for tensor in tensors {
        let byte_len = tensor.values.len() * 4;
        header.insert(
            tensor.name.clone(),
            json!({
                "dtype": "F32",
                "shape": tensor.shape,
                "data_offsets": [offset, offset + byte_len],
            }),
        );
        offset += byte_len;
    }
    let header_bytes = serde_json::to_vec(&Value::Object(header))
        .map_err(|error| format!("serialize safetensors header: {error}"))?;
    let header_len = u64::try_from(header_bytes.len())
        .map_err(|_| "safetensors header length exceeds u64".to_owned())?;
    let mut bytes = Vec::with_capacity(8 + header_bytes.len() + offset);
    bytes.extend_from_slice(&header_len.to_le_bytes());
    bytes.extend_from_slice(&header_bytes);
    for tensor in tensors {
        for value in &tensor.values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    std::fs::write(path, bytes).map_err(|error| format!("write {}: {error}", path.display()))
}

/// A real `tokenizers`-crate tokenizer: word-level model over the fixture
/// vocabulary, whitespace pre-tokenizer, and the standard BERT pair template
/// (`[CLS] query [SEP] doc [SEP]` with type ids 0/1) so the forward sees the
/// same special-token and token-type layout as the real checkpoint.
fn write_tokenizer_json(path: &Path) -> TestResult {
    let mut vocab = serde_json::Map::new();
    vocab.insert("[PAD]".to_owned(), json!(0));
    vocab.insert("[UNK]".to_owned(), json!(1));
    vocab.insert("[CLS]".to_owned(), json!(2));
    vocab.insert("[SEP]".to_owned(), json!(3));
    for (index, word) in VOCAB_WORDS.iter().enumerate() {
        vocab.insert((*word).to_owned(), json!(index + 4));
    }
    let tokenizer = json!({
        "version": "1.0",
        "truncation": null,
        "padding": null,
        "added_tokens": [],
        "normalizer": null,
        "pre_tokenizer": {"type": "Whitespace"},
        "post_processor": {
            "type": "TemplateProcessing",
            "single": [
                {"SpecialToken": {"id": "[CLS]", "type_id": 0}},
                {"Sequence": {"id": "A", "type_id": 0}},
                {"SpecialToken": {"id": "[SEP]", "type_id": 0}},
            ],
            "pair": [
                {"SpecialToken": {"id": "[CLS]", "type_id": 0}},
                {"Sequence": {"id": "A", "type_id": 0}},
                {"SpecialToken": {"id": "[SEP]", "type_id": 0}},
                {"Sequence": {"id": "B", "type_id": 1}},
                {"SpecialToken": {"id": "[SEP]", "type_id": 1}},
            ],
            "special_tokens": {
                "[CLS]": {"id": "[CLS]", "ids": [2], "tokens": ["[CLS]"]},
                "[SEP]": {"id": "[SEP]", "ids": [3], "tokens": ["[SEP]"]},
            },
        },
        "decoder": null,
        "model": {
            "type": "WordLevel",
            "vocab": Value::Object(vocab),
            "unk_token": "[UNK]",
        },
    });
    let bytes = serde_json::to_vec_pretty(&tokenizer)
        .map_err(|error| format!("serialize tokenizer.json: {error}"))?;
    std::fs::write(path, bytes).map_err(|error| format!("write {}: {error}", path.display()))
}

/// Generate the model directory and load it through the REAL production
/// loader: real tokenizer load, real safetensors parse, real int8
/// per-output-channel quantization, real fused-QKV stacking.
fn load_fixture_reranker(dir: &Path) -> Result<NativeReranker, String> {
    write_tokenizer_json(&dir.join("tokenizer.json"))?;
    write_safetensors(&dir.join("model_f32.safetensors"), &fixture_tensors())?;
    NativeReranker::load(dir)
        .map_err(|error| format!("load fixture reranker from {}: {error}", dir.display()))
}

/// One fixture document of exactly `words` vocabulary words. Stride-cycling
/// the vocabulary gives every document a distinct token sequence, so
/// per-document logits are distinct and order mix-ups cannot cancel out.
fn fixture_document(index: usize, words: usize) -> RerankDocument {
    let text = (0..words)
        .map(|word_index| VOCAB_WORDS[(index * 7 + word_index * 3) % VOCAB_WORDS.len()])
        .collect::<Vec<_>>()
        .join(" ");
    RerankDocument {
        doc_id: format!("doc{index:02}"),
        text,
    }
}

/// Exact per-document token count for a fixture document: every whitespace
/// word is one word-level token, plus the query and the pair template's
/// [CLS]/[SEP]/[SEP].
fn tokens_per_document(words: usize) -> usize {
    words + QUERY_WORDS + TEMPLATE_SPECIAL_TOKENS
}

/// Run `work` on a named worker thread and fail with the deadlock-class
/// diagnosis if it does not finish before the deadline. A wedge therefore
/// surfaces as a readable test failure instead of an opaque CI timeout. The
/// deadline is liveness-only slack for slow debug CI hosts — a real deadlock
/// never completes, so any finite bound catches it without flake risk.
fn with_deadline<T: Send + 'static>(
    label: &'static str,
    deadline: Duration,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name(format!("bd-95qe4-{label}"))
        .spawn(move || {
            let _ = sender.send(work());
        })
        .map_err(|error| format!("spawn {label} worker: {error}"))?;
    match receiver.recv_timeout(deadline) {
        Ok(result) => result,
        Err(_) => Err(format!(
            "{label} did not complete within {deadline:?}: the nested-rayon + Mutex deadlock \
             class regressed (bd-95qe4) — a rerank path is holding the session Mutex across a \
             same-pool rayon wait"
        )),
    }
}

/// The liveness deadline. Generous on purpose: it only has to separate
/// "slow debug-profile forward" from "never finishes".
const RERANK_DEADLINE: Duration = Duration::from_secs(600);

fn ensure_well_formed_scores(
    label: &str,
    documents: &[RerankDocument],
    scores: &[RerankScore],
) -> TestResult {
    ensure(
        scores.len() == documents.len(),
        format!(
            "{label}: expected {} scores, got {}",
            documents.len(),
            scores.len()
        ),
    )?;
    for (index, (document, score)) in documents.iter().zip(scores).enumerate() {
        ensure(
            score.doc_id == document.doc_id,
            format!(
                "{label}: doc identity drifted at input rank {index}: {} != {}",
                score.doc_id, document.doc_id
            ),
        )?;
        ensure(
            score.original_rank == index,
            format!(
                "{label}: original_rank for {} is {}, expected {index}",
                score.doc_id, score.original_rank
            ),
        )?;
        ensure(
            score.score.is_finite() && (0.0..=1.0).contains(&score.score),
            format!(
                "{label}: calibrated score for {} is {}, expected finite in [0, 1]",
                score.doc_id, score.score
            ),
        )?;
        ensure(
            score.raw_logit.is_some_and(f32::is_finite),
            format!(
                "{label}: raw logit for {} is {:?}, expected Some(finite)",
                score.doc_id, score.raw_logit
            ),
        )?;
    }
    Ok(())
}

fn ensure_identical_scores(
    label: &str,
    reference: &[RerankScore],
    candidate: &[RerankScore],
) -> TestResult {
    ensure(
        reference.len() == candidate.len(),
        format!(
            "{label}: score count drifted: {} != {}",
            reference.len(),
            candidate.len()
        ),
    )?;
    for (left, right) in reference.iter().zip(candidate) {
        ensure(
            left.doc_id == right.doc_id && left.original_rank == right.original_rank,
            format!(
                "{label}: document identity drifted: {}#{} != {}#{}",
                left.doc_id, left.original_rank, right.doc_id, right.original_rank
            ),
        )?;
        ensure(
            left.score.to_bits() == right.score.to_bits(),
            format!(
                "{label}: calibrated score for {} drifted: {} != {}",
                left.doc_id, left.score, right.score
            ),
        )?;
        ensure(
            left.raw_logit.map(f32::to_bits) == right.raw_logit.map(f32::to_bits),
            format!(
                "{label}: raw logit for {} drifted: {:?} != {:?}",
                left.doc_id, left.raw_logit, right.raw_logit
            ),
        )?;
    }
    Ok(())
}

/// The historical multi-doc hang shape, model-free: 24 documents (three times
/// the historical 8-slot session-pool cap) through one `rerank_sync` call
/// whose total token count exceeds the batched forward's per-chunk budget, so
/// the sequential chunk loop crosses at least one chunk boundary while the
/// session `Mutex` is held and every per-chunk forward fans onto ambient
/// rayon (at ~98 tokens per document the attention softmax rows are far past
/// the kernel's parallelism threshold). Completion is the regression signal;
/// bit-exact score equality across two runs pins the documented determinism
/// contract of the parallel forward.
#[test]
fn many_documents_batched_rerank_completes_without_deadlock() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let reranker = Arc::new(load_fixture_reranker(temp.path())?);
    let document_count = 24;
    let words_per_document = 90;
    let documents: Vec<RerankDocument> = (0..document_count)
        .map(|index| fixture_document(index, words_per_document))
        .collect();
    let total_tokens = document_count * tokens_per_document(words_per_document);
    ensure(
        total_tokens > CHUNK_TOKEN_BUDGET_MIRROR,
        format!(
            "fixture must overflow the per-chunk token budget to exercise the chunk loop: \
             {total_tokens} <= {CHUNK_TOKEN_BUDGET_MIRROR}"
        ),
    )?;

    let run = |label: &'static str| {
        let reranker = Arc::clone(&reranker);
        let documents = documents.clone();
        with_deadline(label, RERANK_DEADLINE, move || {
            reranker
                .rerank_sync(QUERY, &documents)
                .map_err(|error| format!("{label}: rerank_sync failed: {error}"))
        })
    };

    let first = run("first-many-doc-rerank")?;
    ensure_well_formed_scores("first-many-doc-rerank", &documents, &first)?;
    let second = run("second-many-doc-rerank")?;
    ensure_well_formed_scores("second-many-doc-rerank", &documents, &second)?;
    ensure_identical_scores("many-doc determinism", &first, &second)
}

/// The in-process contention shape ee actually ships: several callers share
/// one loaded reranker `Arc` (`SyncRerankerAdapter` holds `Arc<dyn Reranker>`
/// in `src/core/search.rs`) and rerank concurrently, so callers contend on
/// the session `Mutex` while each locked forward fans onto ambient rayon.
/// Every caller must complete (liveness) and produce bit-identical scores to
/// an uncontended reference run (the `Mutex` must serialize, never corrupt or
/// cross-talk).
#[test]
fn concurrent_rerank_callers_share_one_reranker_without_deadlock() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let reranker = Arc::new(load_fixture_reranker(temp.path())?);
    let documents: Vec<RerankDocument> = (0..8).map(|index| fixture_document(index, 40)).collect();

    let reference = {
        let reranker = Arc::clone(&reranker);
        let documents = documents.clone();
        with_deadline("reference-rerank", RERANK_DEADLINE, move || {
            reranker
                .rerank_sync(QUERY, &documents)
                .map_err(|error| format!("reference rerank_sync failed: {error}"))
        })?
    };
    ensure_well_formed_scores("reference-rerank", &documents, &reference)?;

    let caller_count = 4;
    let concurrent_results = {
        let reranker = Arc::clone(&reranker);
        let documents = documents.clone();
        with_deadline("concurrent-rerank", RERANK_DEADLINE, move || {
            let start_together = Arc::new(Barrier::new(caller_count));
            let mut handles = Vec::with_capacity(caller_count);
            for caller in 0..caller_count {
                let reranker = Arc::clone(&reranker);
                let documents = documents.clone();
                let start_together = Arc::clone(&start_together);
                let handle = thread::Builder::new()
                    .name(format!("bd-95qe4-caller-{caller}"))
                    .spawn(move || {
                        start_together.wait();
                        reranker.rerank_sync(QUERY, &documents).map_err(|error| {
                            format!("caller {caller}: rerank_sync failed: {error}")
                        })
                    })
                    .map_err(|error| format!("spawn caller {caller}: {error}"))?;
                handles.push(handle);
            }
            let mut results = Vec::with_capacity(caller_count);
            for handle in handles {
                results.push(
                    handle
                        .join()
                        .map_err(|_| "concurrent caller panicked".to_owned())??,
                );
            }
            Ok(results)
        })?
    };

    for (caller, scores) in concurrent_results.iter().enumerate() {
        ensure_identical_scores(&format!("concurrent caller {caller}"), &reference, scores)?;
    }
    Ok(())
}
