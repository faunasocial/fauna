"""End-to-end probe: the web SPA round-trips `fauna.protocol.echo` over its
browser WS-RPC client (`fauna_rpc_wasm::WsRpcClient`, exposed via
`libs/fauna-wasm`'s `#[wasm_bindgen]` handle and owned by
`apps/fauna-web/src/lib/rpc.ts`).

Validates the web WS-RPC adoption design (tracked internally), § Phase 4:
- the gloo-net subprotocol-bearer WS handshake (`Sec-WebSocket-Protocol:
  fauna.v1, bearer.<token>`) works end-to-end against a real nest (the legacy
  `?token=` push form would 4426 against the current nest, so a successful
  echo proves the WS-RPC handshake landed — plan Risk #3),
- `rpc.ts` owns the singleton `WsRpcClient` and wires the bearer from
  `getAuthToken`,
- `WsRpcClient::request::<EchoRequest, EchoReply>(...)` round-trips a typed
  reply over the dispatcher.

The web twin of `test_sp_linux_ws_rpc_echo.py`. The SPA exposes a
`window.__fauna_rpcEcho(dataHex)` hook (added in Phase 4) that decodes the hex
to bytes, routes `fauna.protocol.echo` through the singleton client, and
resolves `{ok, data_hex, error}`. tier_3: a real `fauna-nest` is spun by the
`logged_in_app` → `nest_instance` fixture chain, and the WS upgrade is proxied
through to it by the `spa_url` fixture.
"""
from __future__ import annotations

import pytest

from helpers.app_surface import declared_absence

# Web-only (the `window.__fauna_rpcEcho` hook is SPA-specific). Deselected under
# non-web `--client` by the conftest marker-platform filter; the in-body
# `is_web()` skip still guards a no-`--client` run.
pytestmark = [pytest.mark.web, pytest.mark.tier_3]


def test_web_ws_rpc_echo_round_trip(logged_in_app):
    """Web round-trips `fauna.protocol.echo` via the singleton WsRpcClient."""
    driver = logged_in_app.driver
    if not driver.is_web():
        declared_absence(
            driver,
            capability="the window.__fauna_rpcEcho automation hook",
            doc="testing.md § Cross-app e2e conventions, point 15 (web's "
            "window.__fauna_* hooks are a web-only automation surface; native "
            "apps carry their own compiled-in TestAgent instead)",
        )

    probe = "hello-from-web"
    expected_hex = probe.encode("utf-8").hex()

    # `__fauna_rpcEcho` returns a Promise that lazily connects the singleton
    # WS-RPC client (subprotocol-bearer handshake), waits for it to come up,
    # then issues the echo. Playwright's `evaluate` awaits the returned
    # Promise, so `eval_js` blocks until the round-trip resolves.
    script = (
        "(async () => {"
        "  if (typeof window.__fauna_rpcEcho !== 'function') {"
        "    return {ok: false, error: 'window.__fauna_rpcEcho hook missing'};"
        "  }"
        f"  return await window.__fauna_rpcEcho({expected_hex!r});"
        "})()"
    )
    result = driver.eval_js(script)

    assert result is not None, (
        "eval_js returned nothing — the SPA didn't evaluate the echo hook"
    )
    assert result.get("ok") is True, (
        f"web WS-RPC echo failed: {result.get('error')!r}"
    )
    assert result.get("data_hex") == expected_hex, (
        f"echo round-trip mismatch: sent hex={expected_hex!r}, "
        f"got hex={result.get('data_hex')!r}"
    )
