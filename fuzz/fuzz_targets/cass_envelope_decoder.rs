#![no_main]

use ee::cass::process::fuzz_decode_cass_stdout_stream;
use libfuzzer_sys::fuzz_target;

const CASS_STDOUT_LINE_MAX_BYTES: usize = 1024 * 1024;
const MAX_INPUT_BYTES: usize = CASS_STDOUT_LINE_MAX_BYTES + 4096;
const MAX_ENVELOPE_TEXT_BYTES: usize = 64 * 1024;
const MAX_EXCERPT_BYTES: usize = 65_536;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    exercise_stream_decoder(data);
    exercise_cass_json_envelopes(data);

    if data.starts_with(b"CASS_CAP_LINE") {
        exercise_cap_sized_line();
    }
    if data.starts_with(b"CASS_OVERSIZE") {
        exercise_oversized_line();
    }
});

fn exercise_stream_decoder(data: &[u8]) {
    let first = fuzz_decode_cass_stdout_stream(data);
    let second = fuzz_decode_cass_stdout_stream(data);
    assert_eq!(first, second);

    match first {
        Ok(summary) => {
            assert!(summary.peak_buffer_bytes <= CASS_STDOUT_LINE_MAX_BYTES + 1);
            assert!(summary.peak_line_bytes <= CASS_STDOUT_LINE_MAX_BYTES);
            assert!(summary.bytes_seen <= data.len());
            assert!(summary.line_count <= data.len().saturating_add(1));
        }
        Err(error) => {
            assert!(!error.kind_str().is_empty());
            assert!(!error.to_string().is_empty());
        }
    }
}

fn exercise_cass_json_envelopes(data: &[u8]) {
    let _ = ee::cass::parse_sessions_json_summary(data);
    let _ = ee::cass::parse_view_json_summary(data, "/tmp/cass-envelope-fuzz.jsonl");

    if data.len() > MAX_ENVELOPE_TEXT_BYTES {
        return;
    }

    let content = String::from_utf8_lossy(data);
    let Ok(content_json) = serde_json::to_string(content.as_ref()) else {
        return;
    };
    let line_number = line_number_from(data);

    let view_json = format!(r#"{{"lines":[{{"line":{line_number},"content":{content_json}}}]}}"#);
    if let Ok(summary) =
        ee::cass::parse_view_json_summary(view_json.as_bytes(), "/tmp/cass-envelope-fuzz.jsonl")
    {
        assert!(summary.accepted_items <= 1);
        assert!(summary.max_line >= 1);
        assert!(summary.max_excerpt_bytes <= MAX_EXCERPT_BYTES);
    }

    let sessions_json = format!(
        r#"{{"sessions":[{{"path":"/tmp/cass-envelope-fuzz.jsonl","agent":"codex","message_count":{line_number},"token_count":{line_number}}}]}}"#
    );
    let _ = ee::cass::parse_sessions_json_summary(sessions_json.as_bytes());
}

fn exercise_cap_sized_line() {
    let mut input = vec![b'x'; CASS_STDOUT_LINE_MAX_BYTES];
    input.push(b'\n');

    let summary = fuzz_decode_cass_stdout_stream(&input)
        .expect("cap-sized newline-terminated line must decode");
    assert_eq!(summary.line_count, 1);
    assert_eq!(summary.bytes_seen, CASS_STDOUT_LINE_MAX_BYTES);
    assert_eq!(summary.peak_line_bytes, CASS_STDOUT_LINE_MAX_BYTES);
    assert!(summary.peak_buffer_bytes <= CASS_STDOUT_LINE_MAX_BYTES + 1);
}

fn exercise_oversized_line() {
    let input = vec![b'x'; CASS_STDOUT_LINE_MAX_BYTES + 1];
    let error = fuzz_decode_cass_stdout_stream(&input)
        .expect_err("oversized unterminated line must be rejected");
    assert_eq!(error.kind_str(), "io");
    assert!(
        error
            .to_string()
            .contains("cass subprocess stdout line exceeded")
    );
}

fn line_number_from(data: &[u8]) -> u32 {
    let mut bytes = [0_u8; 4];
    for (target, source) in bytes.iter_mut().zip(data.iter().copied()) {
        *target = source;
    }
    u32::from_le_bytes(bytes).max(1)
}
