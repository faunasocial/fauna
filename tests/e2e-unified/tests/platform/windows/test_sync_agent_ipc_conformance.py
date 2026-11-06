"""Conformance test: Python dag-cbor IPC encoder vs. Rust golden hex.

Pins the ProvisionCapability request encoding byte-for-byte against the
canonical output of the Rust codec (fauna_cbor::encode_canonical) as emitted
by fauna-ipc/src/sync.rs::print_wire_format_for_provision_capability, and the
ReconcileAccountRuntime nudge against print_wire_format_for_control_plane_ops.

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
# (fauna-ipc/src/sync.rs::print_wire_format_for_provision_capability), byte for
# byte — its `nest_url` is the Rust fixture's own "https://example.com".
# Regenerate it if the struct shape changes:
#   cmd //c "scripts\cargo-win.cmd test -p fauna-ipc print_wire_format_for_provision_capability -- --nocapture"
GOLDEN_PROVISION_PAYLOAD_HEX = "a262696407666d6574686f64a17350726f766973696f6e4361706162696c697479a866626561726572a265746f6b656e63746f6b6a657870697265735f6174187b686163746f725f696458201111111111111111111111111111111111111111111111111111111111111111686e6573745f75726c7368747470733a2f2f6578616d706c652e636f6d696465766963655f6964676465762d3132336a6261636b75705f6b65795820f39582d247fa3bb84a45224943d9f058b8650bed6d6640e3a69165a147383a147170726f766973696f6e65645f61745f6d7300757072656465636573736f725f6163746f725f69647340777072656465636573736f725f6261636b75705f6b65797340"


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
        provisioned_at_ms=0,  # `SyncCapability::new`'s unstamped value
    )
    assert ipc.encode_payload(req).hex() == GOLDEN_PROVISION_PAYLOAD_HEX


# `fauna-ipc/src/sync.rs::print_wire_format_for_control_plane_ops`, the
# `ReconcileAccountRuntime(id=15)` line, split at its CBOR items (the joined
# string is the printed hex verbatim; one unbroken run reads to the publish
# secret scan as key material):
#   cargo test -p fauna-ipc --lib print_wire_format_for_control_plane_ops -- --nocapture
GOLDEN_RECONCILE_ACCOUNT_RUNTIME_PAYLOAD_HEX = "".join((
    "a2",                                # map(2)
    "626964", "0f",                      # "id": 15
    "666d6574686f64",                    # "method":
    "77",                                # text(23)
    "5265636f6e63696c65",                # "Reconcile"
    "4163636f756e74",                    # "Account"
    "52756e74696d65",                    # "Runtime"
))


def test_reconcile_account_runtime_matches_rust_golden():
    req = ipc.reconcile_account_runtime_request(15)
    assert ipc.encode_payload(req).hex() == GOLDEN_RECONCILE_ACCOUNT_RUNTIME_PAYLOAD_HEX


@pytest.mark.host_platform("windows")
def test_a_failed_open_names_its_real_win32_error():
    """A pipe open that fails reports the OS's own error number (convention 6).

    The helper read ``ctypes.get_last_error()`` after a ``ctypes.windll`` call, which
    only a ``use_last_error`` handle feeds, so every failed open said "Win32 error 0"
    whatever the cause. A pipe nothing serves is ``ERROR_FILE_NOT_FOUND`` (2), waited
    out for the open's short deadline and then raised. In-process: no agent, no
    pipe is created.
    """
    import uuid

    unserved = rf"\\.\pipe\fauna-e2e-unserved-{uuid.uuid4().hex}"
    with pytest.raises(OSError) as failed:
        ipc.send_request(unserved, ipc.encode_frame(ipc.get_service_status_request(1)), timeout=0.3)
    assert failed.value.errno == 2, f"expected ERROR_FILE_NOT_FOUND (2), got {failed.value!r}"
    assert "Win32 error 2" in str(failed.value), str(failed.value)
