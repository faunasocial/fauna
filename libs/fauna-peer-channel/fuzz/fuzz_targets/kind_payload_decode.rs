#![no_main]

//! PQ-2 target 3 — arbitrary bytes through strict decode of every peer-leg
//! kind's wire struct (the `fauna-peer-sync` allowlist's params + replies).
//! Must never panic.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = fauna_peer_channel::hardening::check_kind_payload_decode(data);
});
