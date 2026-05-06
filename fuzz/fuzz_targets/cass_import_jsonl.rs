#![no_main]

use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 131_072;
const MAX_EXCERPT_BYTES: usize = 65_536;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let _ = ee::cass::parse_sessions_json_summary(data);
    let _ = ee::cass::parse_view_json_summary(data, "/tmp/cass-session.jsonl");

    let content = String::from_utf8_lossy(data);
    let Ok(content_json) = serde_json::to_string(content.as_ref()) else {
        return;
    };

    let line_number = line_number_from(data);
    let view_json = format!(r#"{{"lines":[{{"line":{line_number},"content":{content_json}}}]}}"#);
    if let Ok(summary) =
        ee::cass::parse_view_json_summary(view_json.as_bytes(), "/tmp/cass-session.jsonl")
    {
        assert!(summary.accepted_items <= 1);
        assert!(summary.max_line >= 1);
        assert!(summary.max_excerpt_bytes <= MAX_EXCERPT_BYTES);
    }

    for path in session_paths(data, content.as_ref()) {
        let Ok(path_json) = serde_json::to_string(path) else {
            continue;
        };
        let sessions_json = format!(
            r#"{{"sessions":[{{"path":{path_json},"agent":"codex","message_count":{line_number},"token_count":{line_number}}}]}}"#
        );
        let _ = ee::cass::parse_sessions_json_summary(sessions_json.as_bytes());
    }
});

fn line_number_from(data: &[u8]) -> u32 {
    let mut bytes = [0_u8; 4];
    for (target, source) in bytes.iter_mut().zip(data.iter().copied()) {
        *target = source;
    }
    u32::from_le_bytes(bytes).max(1)
}

fn session_paths<'a>(data: &[u8], text: &'a str) -> [&'a str; 5] {
    let generated = if data.is_empty() || text.trim().is_empty() {
        "/tmp/generated-cass-session.jsonl"
    } else {
        text
    };
    [
        "/tmp/cass-session.jsonl",
        "-leading-dash.jsonl",
        " whitespace.jsonl ",
        "nul\0path.jsonl",
        generated,
    ]
}
