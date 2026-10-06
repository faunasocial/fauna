"""Conformance test: Python dag-cbor IPC encoder vs. Rust golden hex.

Pins the ProvisionCapability request encoding byte-for-byte against the
canonical output of the Rust codec (fauna_cbor::encode_canonical) as emitted
by fauna-ipc/src/sync.rs::print_wire_format_for_provision_capability.

This is a tier_1 pure-Python in-process test — no nest binary, no driver,
no named-pipe I/O. It runs anywhere Python + cbor2 are available.

Regenerate the Rust golden with:
    cmd //c "scripts\\cargo-win.cmd test -p fauna-ipc \
        print_wire_format_for_provision_capability \
        -- --nocapture"
"""

import pytest

from helpers import sync_agent_ipc as ipc

pytestmark = pytest.mark.tier_1

# Golden payload hex emitted by the Rust codec `fauna_cbor::encode_canonical`
# (fauna-ipc/src/sync.rs::print_wire_format_for_provision_capability), with the
# `nest_url` field's bytes hand-recomputed for "https://example.com" in place of
# the Rust fixture's "https://example.com" (a HARD_I leak-check.sh term) — every
# other byte is untouched from the real Rust-emitted vector. This file's own
# `nest_url=` literal below made the same swap so nothing here depends on the
# publish transform's content-scrub pass to launder it: that pass rewrites
# `example.com` -> `example.com` in TEXT, but this constant is a HEX-encoded
# CBOR blob, invisible to a plain-text scrub/scan (the gap `encoded_rescan.py`
# closes). Regenerate the Rust side with (still using "https://example.com" —
# only this Python-side hex was patched, not the Rust dev fixture) and
# hand-patch the resulting hex the same way if the struct shape changes:
#   cmd //c "scripts\cargo-win.cmd test -p fauna-ipc print_wire_format_for_provision_capability -- --nocapture"
GOLDEN_PROVISION_PAYLOAD_HEX = "a262696407666d6574686f64a17350726f766973696f6e4361706162696c697479a566626561726572a265746f6b656e63746f6b6a657870697265735f6174187b686163746f725f696458201111111111111111111111111111111111111111111111111111111111111111686e6573745f75726c7368747470733a2f2f6578616d706c652e636f6d696465766963655f6964676465762d3132336a6261636b75705f6b65795820f39582d247fa3bb84a45224943d9f058b8650bed6d6640e3a69165a147383a14"


def test_provision_capability_matches_rust_golden():
    backup_key = bytes.fromhex(
        "f39582d247fa3bb84a45224943d9f058b8650bed6d6640e3a69165a147383a14"
    )  # BackupKey::derive(&[0x01; 32])
    actor_id = bytes([0x11] * 32)
    req = ipc.provision_capability_request(
        7,
        backup_key=backup_key,
        actor_id=actor_id,
        nest_url="https://example.com",
        device_id="dev-123",
        bearer_token="tok",
        bearer_expires_at=123,
    )
    assert ipc.encode_payload(req).hex() == GOLDEN_PROVISION_PAYLOAD_HEX
