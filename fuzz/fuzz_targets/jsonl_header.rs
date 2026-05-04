#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > 65_536 {
        return;
    }
    let input = String::from_utf8_lossy(data);
    let _ = ee::core::jsonl_import::parse_jsonl_header_line(&input);
});
