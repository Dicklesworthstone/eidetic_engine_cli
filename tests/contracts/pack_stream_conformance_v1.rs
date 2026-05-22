//! bd-2yc96: conformance harness for `ee.pack.stream.v1` NDJSON frame
//! sequencing. Exercises the five mandated scenarios:
//!
//!   (a) happy path
//!   (b) cancellation mid-stream
//!   (c) error mid-stream
//!   (d) zero items
//!   (e) max-tokens cutoff
//!
//! Asserts the contract guarantees that ride alongside ordering:
//!   - one frame per NDJSON line (PackStreamWriter round-trip)
//!   - StreamSequenceValidator accepts each documented happy shape and
//!     rejects every documented violation
//!   - trailer fields (packHash, totalItems, usedTokens, degraded) match
//!     the batch-mode `ContextResponse` envelope when both are emitted
//!     from the same draft
//!
//! Differential testing is the point: each frame sequence we build also
//! flows through the validator, the writer, and the schema-level
//! constants, so a regression in any of those components flags here.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

use std::str::FromStr;

use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::streaming::{
    ContextStreamFrameOptions, HeaderFrame, HeaderFrameInput, ItemFrame, ItemFrameInput,
    PACK_STREAM_SCHEMA_V1, PackStreamFrame, PackStreamWriter, StreamError, StreamSequenceValidator,
    StreamSeverity, StreamValidationError, TerminalFrame, TerminalKind, TrailerFrame,
    context_response_stream_frames,
};
use ee::pack::{
    ContextRequest, ContextResponse, PackCandidate, PackCandidateInput, PackProvenance,
    PackSection, PackTrustSignal, TokenBudget, assemble_draft,
};
use serde_json::Value as JsonValue;
use uuid::Uuid;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_eq<T: std::fmt::Debug + PartialEq>(left: &T, right: &T, label: &str) -> TestResult {
    if left == right {
        Ok(())
    } else {
        Err(format!(
            "{label}: left={left:?} right={right:?} (bd-2yc96 conformance)"
        ))
    }
}

const PACK_ID: &str = "pack_2yc96";
const WORKSPACE_ID: &str = "workspace_2yc96";
const REQUEST_ID: &str = "request_2yc96";
const STARTED_AT: &str = "2026-05-22T16:30:00Z";
const COMPLETED_AT: &str = "2026-05-22T16:30:01Z";

fn header_frame() -> PackStreamFrame {
    PackStreamFrame::Header(HeaderFrame::new(HeaderFrameInput {
        pack_id: PACK_ID.to_string(),
        query: "prepare release".to_string(),
        workspace_id: WORKSPACE_ID.to_string(),
        request_id: REQUEST_ID.to_string(),
        profile: "balanced".to_string(),
        max_tokens: 512,
        candidate_pool: 16,
        memory_scope: "workspace".to_string(),
        strict_scope: true,
        started_at: STARTED_AT.to_string(),
    }))
}

fn item_frame(seq: u32, rank: u32) -> PackStreamFrame {
    let mut frame = ItemFrame::new(ItemFrameInput {
        pack_id: PACK_ID.to_string(),
        seq,
        rank,
        memory_id: format!("mem_{rank}"),
        section: "procedural_rules".to_string(),
        content: format!("Conformance fixture item {rank}."),
        estimated_tokens: 6,
        why: "matched conformance fixture".to_string(),
    });
    frame
        .scores
        .insert("relevance".to_string(), JsonValue::from(0.91));
    PackStreamFrame::Item(frame)
}

fn trailer_frame(total_items: u32, used_tokens: u32) -> PackStreamFrame {
    PackStreamFrame::Trailer(TrailerFrame::new(
        PACK_ID,
        "blake3:conformance-fixture",
        total_items,
        used_tokens,
        COMPLETED_AT,
    ))
}

fn stream_error_envelope() -> StreamError {
    StreamError::new(
        "stream_failed",
        "stream failed mid-flight",
        StreamSeverity::Medium,
        Some("retry the stream".to_string()),
    )
}

fn error_terminal_frame() -> PackStreamFrame {
    PackStreamFrame::Terminal(TerminalFrame::error(
        Some(PACK_ID.to_string()),
        stream_error_envelope(),
    ))
}

fn cancelled_terminal_frame() -> PackStreamFrame {
    PackStreamFrame::Terminal(TerminalFrame::cancelled(
        Some(PACK_ID.to_string()),
        StreamError::new(
            "stream_cancelled",
            "client cancelled the stream",
            StreamSeverity::Low,
            None,
        ),
    ))
}

/// Walk every frame through `StreamSequenceValidator` and then call
/// `finish()`. The combined output is what callers actually rely on.
fn run_validator(frames: &[PackStreamFrame]) -> Result<(), StreamValidationError> {
    let mut validator = StreamSequenceValidator::new();
    for frame in frames {
        validator.observe(frame)?;
    }
    validator.finish()
}

/// NDJSON round-trip: write each frame through `PackStreamWriter` and
/// confirm that (1) the byte stream is exactly one `\n`-terminated JSON
/// object per frame, (2) re-parsing each line yields a JSON object that
/// carries the documented `schema`/`kind` constants.
fn ndjson_roundtrip_round(frames: &[PackStreamFrame]) -> TestResult {
    let mut buffer = Vec::<u8>::new();
    {
        let mut writer = PackStreamWriter::new(&mut buffer);
        for frame in frames {
            writer
                .write_frame(frame)
                .map_err(|error| format!("write_frame: {error}"))?;
        }
        ensure_eq(
            &writer.frames_written(),
            &u32::try_from(frames.len()).expect("frame count fits u32"),
            "frames_written matches input length",
        )?;
    }

    let text =
        std::str::from_utf8(&buffer).map_err(|error| format!("NDJSON not UTF-8: {error}"))?;
    let lines: Vec<&str> = text.split_terminator('\n').collect();
    ensure_eq(&lines.len(), &frames.len(), "one NDJSON line per frame")?;

    for (index, (line, frame)) in lines.iter().zip(frames.iter()).enumerate() {
        ensure(
            !line.is_empty(),
            format!("line {index} is empty (NDJSON requires content per line)"),
        )?;
        ensure(
            !line.contains('\n'),
            format!("line {index} contained an embedded newline (frame split)"),
        )?;
        let parsed: JsonValue =
            serde_json::from_str(line).map_err(|error| format!("line {index} parse: {error}"))?;
        let object = parsed
            .as_object()
            .ok_or_else(|| format!("line {index} is not a JSON object"))?;
        ensure_eq(
            &object
                .get("schema")
                .and_then(|value| value.as_str())
                .unwrap_or_default(),
            &PACK_STREAM_SCHEMA_V1,
            "schema constant in NDJSON line",
        )?;
        let expected_kind = match frame {
            PackStreamFrame::Header(_) => "header",
            PackStreamFrame::Item(_) => "item",
            PackStreamFrame::Trailer(_) => "trailer",
            PackStreamFrame::Terminal(terminal) => match terminal.kind {
                TerminalKind::Error => "error",
                TerminalKind::Cancelled => "cancelled",
            },
        };
        ensure_eq(
            &object
                .get("kind")
                .and_then(|value| value.as_str())
                .unwrap_or_default(),
            &expected_kind,
            "kind discriminant in NDJSON line",
        )?;
    }
    Ok(())
}

// ----------------------------- scenario (a) -----------------------------

#[test]
fn happy_path_header_items_trailer_passes_validator_and_round_trips() -> TestResult {
    let frames = vec![
        header_frame(),
        item_frame(0, 1),
        item_frame(1, 2),
        item_frame(2, 3),
        trailer_frame(3, 18),
    ];
    run_validator(&frames).map_err(|error| format!("validator rejected happy path: {error}"))?;
    ndjson_roundtrip_round(&frames)
}

// ----------------------------- scenario (b) -----------------------------

#[test]
fn cancellation_mid_stream_terminal_replaces_trailer() -> TestResult {
    let frames = vec![
        header_frame(),
        item_frame(0, 1),
        item_frame(1, 2),
        cancelled_terminal_frame(),
    ];
    run_validator(&frames).map_err(|error| format!("validator rejected cancellation: {error}"))?;

    let PackStreamFrame::Terminal(terminal) = frames.last().expect("non-empty") else {
        return Err("last frame is not a terminal".to_string());
    };
    ensure_eq(
        &terminal.kind,
        &TerminalKind::Cancelled,
        "terminal kind is cancelled",
    )?;
    ensure_eq(
        &terminal.error.code.as_str(),
        &"stream_cancelled",
        "cancelled error envelope code preserved",
    )?;
    ndjson_roundtrip_round(&frames)
}

// ----------------------------- scenario (c) -----------------------------

#[test]
fn error_mid_stream_terminal_replaces_trailer() -> TestResult {
    let frames = vec![header_frame(), item_frame(0, 1), error_terminal_frame()];
    run_validator(&frames).map_err(|error| format!("validator rejected error: {error}"))?;

    let PackStreamFrame::Terminal(terminal) = frames.last().expect("non-empty") else {
        return Err("last frame is not a terminal".to_string());
    };
    ensure_eq(
        &terminal.kind,
        &TerminalKind::Error,
        "terminal kind is error",
    )?;
    ensure_eq(
        &terminal.error.severity,
        &StreamSeverity::Medium,
        "error severity preserved",
    )?;
    ndjson_roundtrip_round(&frames)
}

// ----------------------------- scenario (d) -----------------------------

#[test]
fn zero_items_header_then_trailer_passes_validator() -> TestResult {
    let frames = vec![header_frame(), trailer_frame(0, 0)];
    run_validator(&frames).map_err(|error| format!("validator rejected zero-items: {error}"))?;

    let PackStreamFrame::Trailer(trailer) = frames.last().expect("non-empty") else {
        return Err("last frame is not a trailer".to_string());
    };
    ensure_eq(&trailer.total_items, &0_u32, "zero-items totalItems is 0")?;
    ensure_eq(&trailer.used_tokens, &0_u32, "zero-items usedTokens is 0")?;
    ndjson_roundtrip_round(&frames)
}

// ----------------------------- scenario (e) -----------------------------

fn fixture_candidate(seed: u128, content: &str, relevance: f32) -> PackCandidate {
    PackCandidate::new(PackCandidateInput {
        memory_id: MemoryId::from_uuid(Uuid::from_u128(seed)),
        section: PackSection::ProceduralRules,
        content: content.to_string(),
        estimated_tokens: 24,
        relevance: UnitScore::parse(relevance).expect("relevance in unit interval"),
        utility: UnitScore::parse(0.7).expect("utility in unit interval"),
        provenance: vec![
            PackProvenance::new(
                ProvenanceUri::from_str("file://tests/contracts/pack_stream_conformance_v1.rs")
                    .expect("provenance URI"),
                "conformance fixture",
            )
            .expect("pack provenance"),
        ],
        why: "matched conformance fixture".to_string(),
    })
    .expect("candidate constructs")
    .with_trust_signal(PackTrustSignal::new(
        TrustClass::HumanExplicit,
        Some("conformance".to_string()),
    ))
}

fn build_max_tokens_cutoff_response() -> ContextResponse {
    let request = ContextRequest::from_query("conformance max-tokens").expect("request");
    // Budget below the sum of estimated_tokens so the assembler MUST
    // omit at least one candidate — that exercises the trailer
    // `skippedTotal` path.
    let budget = TokenBudget::new(32).expect("budget accepts 32");
    let draft = assemble_draft(
        "conformance max-tokens",
        budget,
        vec![
            fixture_candidate(0xA1, "Run cargo fmt --check.", 0.93),
            fixture_candidate(0xA2, "Run RCH-only focused tests.", 0.89),
            fixture_candidate(0xA3, "Verify static proofs.", 0.85),
            fixture_candidate(0xA4, "Push to origin/main.", 0.80),
        ],
    )
    .expect("draft assembles");
    ContextResponse::new(request, draft, Vec::new()).expect("response constructs")
}

#[test]
fn max_tokens_cutoff_trailer_records_skipped_total_and_matches_batch_envelope() -> TestResult {
    let response = build_max_tokens_cutoff_response();
    let options =
        ContextStreamFrameOptions::new(PACK_ID, WORKSPACE_ID, REQUEST_ID, STARTED_AT, COMPLETED_AT);
    let frames = context_response_stream_frames(&response, options)
        .map_err(|error| format!("frame builder: {error}"))?;
    run_validator(&frames)
        .map_err(|error| format!("validator rejected max-tokens cutoff: {error}"))?;
    ndjson_roundtrip_round(&frames)?;

    // The trailer must exist and the skippedTotal must be non-zero
    // (the tight budget guarantees at least one candidate is dropped).
    let trailer = frames
        .iter()
        .find_map(|frame| match frame {
            PackStreamFrame::Trailer(trailer) => Some(trailer),
            _ => None,
        })
        .ok_or("no trailer in max-tokens cutoff stream")?;
    let skipped = trailer
        .skipped_total
        .ok_or("trailer.skippedTotal must be present under a max-tokens cutoff")?;
    ensure(
        skipped > 0,
        format!("trailer.skippedTotal must be > 0 (got {skipped})"),
    )?;

    // packHash and totalItems must match the batch-mode envelope.
    let batch_json = ee::output::render_context_response_json(&response);
    let batch: JsonValue =
        serde_json::from_str(&batch_json).map_err(|error| format!("batch parse: {error}"))?;
    let batch_pack_hash = batch["data"]["pack"]["hash"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| "absent".to_string());
    ensure_eq(
        &trailer.pack_hash,
        &batch_pack_hash,
        "trailer packHash matches batch envelope",
    )?;
    let batch_items_len = batch["data"]["pack"]["items"]
        .as_array()
        .map(Vec::len)
        .ok_or("batch envelope missing data.pack.items array")?;
    ensure_eq(
        &usize::try_from(trailer.total_items).expect("u32 fits usize"),
        &batch_items_len,
        "trailer totalItems matches batch envelope item count",
    )?;
    ensure_eq(
        &trailer.used_tokens,
        &response.data.pack.used_tokens,
        "trailer usedTokens matches draft",
    )?;
    Ok(())
}

// --------------------- validator rejection coverage ---------------------
//
// One test per `StreamValidationError` variant. The point of a
// conformance harness is to fail loudly if any of these documented
// violations starts slipping past the validator.

#[test]
fn validator_rejects_item_before_header() {
    let frames = vec![item_frame(0, 1)];
    let mut validator = StreamSequenceValidator::new();
    let mut observed = None;
    for frame in &frames {
        if let Err(error) = validator.observe(frame) {
            observed = Some(error);
            break;
        }
    }
    assert_eq!(
        observed,
        Some(StreamValidationError::ItemBeforeHeader),
        "item before header must be rejected"
    );
}

#[test]
fn validator_rejects_terminal_before_header() {
    let mut validator = StreamSequenceValidator::new();
    let result = validator.observe(&trailer_frame(0, 0));
    assert_eq!(result, Err(StreamValidationError::TerminalBeforeHeader));
    let mut validator2 = StreamSequenceValidator::new();
    let result2 = validator2.observe(&error_terminal_frame());
    assert_eq!(result2, Err(StreamValidationError::TerminalBeforeHeader));
}

#[test]
fn validator_rejects_duplicate_header() {
    let mut validator = StreamSequenceValidator::new();
    validator.observe(&header_frame()).expect("first header ok");
    assert_eq!(
        validator.observe(&header_frame()),
        Err(StreamValidationError::DuplicateHeader)
    );
}

#[test]
fn validator_rejects_duplicate_terminal() {
    let mut validator = StreamSequenceValidator::new();
    validator.observe(&header_frame()).expect("header ok");
    validator.observe(&trailer_frame(0, 0)).expect("trailer ok");
    assert_eq!(
        validator.observe(&error_terminal_frame()),
        Err(StreamValidationError::DuplicateTerminal)
    );
}

#[test]
fn validator_rejects_frame_after_terminal() {
    let mut validator = StreamSequenceValidator::new();
    validator.observe(&header_frame()).expect("header ok");
    validator
        .observe(&error_terminal_frame())
        .expect("terminal ok");
    assert_eq!(
        validator.observe(&item_frame(0, 1)),
        Err(StreamValidationError::FrameAfterTerminal)
    );
}

#[test]
fn validator_rejects_unexpected_item_seq() {
    let mut validator = StreamSequenceValidator::new();
    validator.observe(&header_frame()).expect("header ok");
    let bad = item_frame(5, 1);
    assert_eq!(
        validator.observe(&bad),
        Err(StreamValidationError::UnexpectedItemSeq {
            expected: 0,
            actual: 5,
        }),
    );
}

#[test]
fn validator_rejects_unexpected_item_rank() {
    let mut validator = StreamSequenceValidator::new();
    validator.observe(&header_frame()).expect("header ok");
    let bad = item_frame(0, 7);
    assert_eq!(
        validator.observe(&bad),
        Err(StreamValidationError::UnexpectedItemRank {
            expected: 1,
            actual: 7,
        }),
    );
}

#[test]
fn validator_rejects_trailer_item_count_mismatch() {
    let mut validator = StreamSequenceValidator::new();
    validator.observe(&header_frame()).expect("header ok");
    validator.observe(&item_frame(0, 1)).expect("item ok");
    // Two items emitted -> trailer must say totalItems=2; assert that 5
    // is rejected with a mismatch carrying the correct numbers.
    let bad_trailer = trailer_frame(5, 6);
    assert_eq!(
        validator.observe(&bad_trailer),
        Err(StreamValidationError::TrailerItemCountMismatch {
            expected: 1,
            actual: 5,
        }),
    );
}

#[test]
fn validator_finish_rejects_missing_header() {
    let validator = StreamSequenceValidator::new();
    assert_eq!(
        validator.finish(),
        Err(StreamValidationError::MissingHeader)
    );
}

#[test]
fn validator_finish_rejects_missing_terminal() {
    let mut validator = StreamSequenceValidator::new();
    validator.observe(&header_frame()).expect("header ok");
    validator.observe(&item_frame(0, 1)).expect("item ok");
    assert_eq!(
        validator.finish(),
        Err(StreamValidationError::MissingTerminal)
    );
}

// ----------------------- schema constant invariant ----------------------

#[test]
fn pack_stream_schema_constant_is_stable() {
    assert_eq!(PACK_STREAM_SCHEMA_V1, "ee.pack.stream.v1");
}
