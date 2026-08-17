#![no_main]

//! T2.7 (`bd-tc-epic-qzk7o.3.9`) — untrusted bootstrap envelopes must not panic.

use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 8 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let _ = ee::mesh::bootstrap_envelope::decode_envelope(data);
    let _ = ee::mesh::transport_session::decode_session_open(data);
    let _ = ee::mesh::transport_session::decode_session_confirm(data);
    let _ = ee::mesh::transport_session::decode_session_finish(data);
});
