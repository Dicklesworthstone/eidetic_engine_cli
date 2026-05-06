#![no_main]

use libfuzzer_sys::fuzz_target;
use serde_json::Value;

const MAX_INPUT_BYTES: usize = 131_072;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(input) else {
        return;
    };

    let Ok(query) = ee::models::query::parse_eql_query(&value) else {
        return;
    };

    assert!(query.limit > 0);
    let metadata = serde_json::json!({
        "workspace": "workspace-a",
        "level": "procedural",
        "kind": "rule",
        "scope": ["repo", "release"],
        "tags": ["release", "format", "unicode-\u{03c0}"],
        "confidence": 0.91,
        "createdAt": "2026-05-06T00:00:00Z",
        "ageDays": 2,
        "graph": {
            "center": "mem_00000000000000000000000000",
            "hops": 2,
            "relations": ["supports", "derived_from"]
        }
    });
    let sparse_metadata = serde_json::json!({
        "tags": "release",
        "created_at": "2026-05-01T00:00:00Z"
    });
    let empty_metadata = serde_json::json!({});
    let candidates = [metadata, sparse_metadata, empty_metadata];

    let _ = query.metadata_filters();
    for candidate in &candidates {
        let _ = query.matches_metadata(Some(candidate));
    }
    let selected = query.execute_metadata(candidates.iter());
    assert!(selected.len() <= candidates.len());
});
