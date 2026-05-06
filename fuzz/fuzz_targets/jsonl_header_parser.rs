#![no_main]

use ee::core::jsonl_import::{JsonlHeaderParseError, parse_jsonl_header};
use libfuzzer_sys::fuzz_target;

const MAX_HEADER_BYTES: usize = 10 * 1024 * 1024 + 4096;
const MAX_LOSSY_BYTES: usize = 256 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_HEADER_BYTES {
        return;
    }

    if let Ok(input) = std::str::from_utf8(data) {
        exercise_header_parser(input);
        if let Some(first_line) = input.lines().next()
            && first_line.len() != input.len()
        {
            exercise_header_parser(first_line);
        }
    } else if data.len() <= MAX_LOSSY_BYTES {
        let input = String::from_utf8_lossy(data);
        exercise_header_parser(input.as_ref());
    }
});

fn exercise_header_parser(input: &str) {
    match parse_jsonl_header(input) {
        Ok(header) => {
            assert_eq!(header.schema, ee::models::EXPORT_HEADER_SCHEMA_V1);

            let serialized =
                serde_json::to_string(&header).expect("valid export header must serialize");
            let reparsed =
                parse_jsonl_header(&serialized).expect("serialized export header must parse");
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
