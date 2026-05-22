#![no_main]

use ee::core::jsonl_import::{JsonlHeaderParseError, parse_jsonl_header};
use ee::models::ExportHeader;
use libfuzzer_sys::fuzz_target;

const MAX_HEADER_BYTES: usize = 10 * 1024 * 1024 + 4096;
const MAX_LOSSY_BYTES: usize = 256 * 1024;
const MAX_DERIVED_TEXT_CHARS: usize = 64;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_HEADER_BYTES {
        return;
    }

    if let Ok(input) = std::str::from_utf8(data) {
        exercise_header_parser(input);
        exercise_first_jsonl_line(input);
    } else if data.len() <= MAX_LOSSY_BYTES {
        let input = String::from_utf8_lossy(data);
        exercise_header_parser(input.as_ref());
        exercise_first_jsonl_line(input.as_ref());
    }

    exercise_generated_header(data);
});

fn exercise_header_parser(input: &str) {
    match parse_jsonl_header(input) {
        Ok(header) => {
            assert_eq!(header.schema, ee::models::EXPORT_HEADER_SCHEMA_V1);

            let serialized = serde_json::to_string(&header);
            assert!(
                serialized.is_ok(),
                "valid export header must serialize: {serialized:?}"
            );
            let Ok(serialized) = serialized else {
                return;
            };
            let reparsed = parse_jsonl_header(&serialized);
            assert!(
                reparsed.is_ok(),
                "serialized export header must parse: {reparsed:?}"
            );
            let Ok(reparsed) = reparsed else {
                return;
            };
            assert_eq!(header, reparsed);
        }
        Err(JsonlHeaderParseError::EmptyLine) => {
            assert!(input.trim().is_empty());
        }
        Err(
            JsonlHeaderParseError::InvalidJson { message }
            | JsonlHeaderParseError::InvalidHeader { message },
        ) => {
            assert!(!message.is_empty());
        }
        Err(JsonlHeaderParseError::MissingSchema) => {
            assert!(!input.trim().is_empty());
        }
        Err(JsonlHeaderParseError::WrongSchema { schema }) => {
            assert!(!schema.trim().is_empty());
            assert_ne!(schema, ee::models::EXPORT_HEADER_SCHEMA_V1);
        }
    }
}

fn exercise_first_jsonl_line(input: &str) {
    let Some((first_line, _)) = input.split_once('\n') else {
        return;
    };
    exercise_header_parser(first_line.trim_end_matches('\r'));
}

fn exercise_generated_header(data: &[u8]) {
    let suffix = data_signature(data);
    let derived_text = derived_json_text(data);
    let record_count = record_count_from(data);

    let Ok(header) = ExportHeader::builder()
        .created_at("2026-05-22T00:00:00Z")
        .ee_version(format!("fuzz-{suffix}"))
        .export_id(format!("exp-{suffix}"))
        .workspace_path(format!("/tmp/{derived_text}"))
        .record_count(record_count)
        .build()
    else {
        return;
    };

    let Ok(serialized) = serde_json::to_string(&header) else {
        return;
    };
    exercise_header_parser(&serialized);

    let jsonl = format!("{serialized}\n{{\"schema\":\"ee.export.memory.v1\"}}");
    exercise_first_jsonl_line(&jsonl);
}

fn derived_json_text(data: &[u8]) -> String {
    let text = String::from_utf8_lossy(data);
    let mut value = String::new();
    for character in text.chars().take(MAX_DERIVED_TEXT_CHARS) {
        if character.is_control() {
            value.push('_');
        } else {
            value.push(character);
        }
    }
    if value.trim().is_empty() {
        "empty".to_string()
    } else {
        value
    }
}

fn record_count_from(data: &[u8]) -> u64 {
    let mut bytes = [0_u8; 8];
    for (target, source) in bytes.iter_mut().zip(data.iter().copied()) {
        *target = source;
    }
    u64::from_le_bytes(bytes)
}

fn data_signature(data: &[u8]) -> String {
    let mut value = String::new();
    for byte in data.iter().take(8) {
        value.push_str(&format!("{byte:02x}"));
    }
    if value.is_empty() {
        "empty".to_string()
    } else {
        value
    }
}
