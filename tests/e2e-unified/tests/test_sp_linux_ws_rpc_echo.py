"""End-to-end probe: Linux `fauna-desktop` round-trips `fauna.protocol.echo`
over its long-lived WS-RPC `NestClient`.

Validates the linux WS-RPC adoption design (tracked internally), § Step 4:
- subprotocol-bearer WS handshake works end-to-end (legacy `?token=` form
  would 4426 against the current nest, so a successful echo proves the
  handshake landed),
- `FaunaClient::nest_rpc()` is wired,
- `NestClient::request::<EchoRequest, EchoReply>(...)` returns a typed
  reply.

The test-agent `rpc_echo` branch dispatches `fauna.protocol.echo` on the
runtime; the reply (hex of `EchoReply::data`) is surfaced via
`state.rpc_echo_reply` for the assertion here.

**linux-only by construction, and now marked as such.** `rpc_echo` is a linux
test-agent command (`apps/fauna-linux/src/test_agent.rs`) — no other app
implements it, and web has its own `test_web_ws_rpc_echo.py`. The file carried
**no app marker** until 2026-07-30, so it collected on all six apps and was red
on the five that cannot answer `rpc_echo`: it polls `state.rpc_echo_reply` for
15s and fails with "never populated", a message that reads like a WS-RPC
regression rather than a command the app never had. Found from tui, where the
convention-11 refusal surface made the dropped command visible. Same class as
`test_contacts.py`, which was red on tui for as long as its assertion existed
for exactly this reason — the `_linux` filename suffix, the test name and this
docstring all declared the scope that the marker did not.
"""
from __future__ import annotations

import time

import pytest

pytestmark = [pytest.mark.tier_2, pytest.mark.linux]


def test_linux_ws_rpc_echo_round_trip(logged_in_app):
    """Linux round-trips `fauna.protocol.echo` via the typed NestClient."""
    driver = logged_in_app.driver

    # `start_ws_rpc` is fired from the AuthSuccess arm after `logged_in_app`'s
    # `set_state` builds the main window. The supervisor handshake takes a
    # few hundred ms on a warm runtime; poll for the reply rather than
    # baking in a fixed sleep.
    probe = "hello-from-linux"
    expected_hex = probe.encode("utf-8").hex()

    driver.call_command("rpc_echo", {"data": probe})

    deadline = time.monotonic() + 15.0
    reply = None
    while time.monotonic() < deadline:
        state = driver.get_state() or {}
        reply = state.get("rpc_echo_reply")
        if reply is not None:
            break
        time.sleep(0.2)

    assert reply is not None, (
        "rpc_echo_reply never populated within 15s — WS-RPC didn't return; "
        f"last state keys: {list((driver.get_state() or {}).keys())}"
    )
    assert reply.get("ok") is True, (
        f"WS-RPC echo failed: {reply.get('error')!r}"
    )
    assert reply.get("data_hex") == expected_hex, (
        f"echo round-trip mismatch: sent hex={expected_hex!r}, "
        f"got hex={reply.get('data_hex')!r}"
    )
