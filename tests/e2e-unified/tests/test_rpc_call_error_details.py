"""``RpcCallError`` must never silently drop the nest's ``details`` string.

The nest attaches an operator-debug string to every internal error and sends
it on the wire (``rpc_errors.rs``). A later change fixed the Rust client
(`libs/fauna-client`) logging it instead of discarding it, because
a handler with several `map_err(internal)` sources (then `fauna.config.put`, since
retired with the `__config` rail; see `config-dissolution.md`) otherwise had them all
arrive as the identical `fauna.protocol.internal` string — undiagnosable.

This harness's own WS-RPC client (`clients/_ws_rpc_core.py`) had the same
bug: `_build_rpc_call_error` extracts `details` from the reply payload and
sets it as an attribute, but the exception's message — the only thing a
failing test's traceback actually shows — never included it. Found triaging
a setup-ERROR cluster: eight
`fauna.protocol.internal` failures under a batched sweep, every traceback
showing only the code, none showing which of the three sources it was.
"""

import pytest

from clients._ws_rpc_core import RpcCallError, _build_rpc_call_error

pytestmark = pytest.mark.tier_1


def test_details_appear_in_the_exception_message_when_present():
    err = RpcCallError(
        code="fauna.protocol.internal",
        message_key="error.protocol.internal",
        message_args={},
        details="blob-store write failed: disk quota exceeded",
    )
    assert "blob-store write failed: disk quota exceeded" in str(err)


def test_no_details_noise_when_the_nest_sent_none():
    err = RpcCallError(
        code="fauna.bridges.permission_denied",
        message_key="error.bridges.permission_denied",
        message_args={},
        details=None,
    )
    assert "details=" not in str(err)


def test_build_rpc_call_error_threads_details_from_the_reply_payload_into_the_message():
    """End-to-end through the actual parser, not just the constructor —
    proves the wire-shape extraction and the message formatting compose."""
    reply_payload = {
        "code": "fauna.protocol.internal",
        "message": {"key": "error.protocol.internal", "args": {}},
        "details": "reserved-folder lookup failed",
    }
    err = _build_rpc_call_error(reply_payload)
    assert isinstance(err, RpcCallError)
    assert "reserved-folder lookup failed" in str(err)
