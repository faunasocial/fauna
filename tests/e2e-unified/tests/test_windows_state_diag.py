"""Diagnostic tests for the Windows state protocol chain.

Tests each link in the chain independently:
  1. Bridge /app/commands POST + GET (Python ↔ bridge)
  2. Bridge /app/state POST + GET (simulating what the app would do)
  3. App test agent polling (bridge → app → bridge round-trip)
  4. JSON field name casing (camelCase vs snake_case mismatch)

Run with: pytest tests/test_windows_state_diag.py -v -k windows
"""

import json
import platform
import time
import urllib.error
import urllib.request

import pytest

from helpers.app_surface import declared_absence


pytestmark = [pytest.mark.tier0, pytest.mark.skipif(
        platform.system() != "Windows",
        reason="Windows-only: requires FlaUI bridge and WinUI app",
    ), pytest.mark.tier_2]


def _require_http_bridge_commands(driver) -> None:
    """Skip unless this app is driven through the native HTTP bridge command
    queue (`POST /app/commands` + `/app/state`) these diagnostics probe.

    Web is driven through `eval_js`/`window.__fauna_*` JS hooks instead — a
    permanent, architecture-level split, not a parity gap (testing.md §
    Cross-app e2e conventions, point 15)."""
    if driver.is_web():
        declared_absence(
            driver,
            capability="the native HTTP bridge command queue "
            "(POST /app/commands, /app/state)",
            doc="testing.md § Cross-app e2e conventions, point 15 (web is "
            "driven via window.__fauna_* JS hooks, not the native "
            "app/HTTP-bridge test agent)",
        )


# ── 1. Bridge endpoints work at all ──────────────────────────────────────────

def test_bridge_health(app):
    """Bridge /health returns ready=true."""
    resp = app.driver._get("/health")
    assert resp.get("ready") is True, f"Bridge not ready: {resp}"


def test_bridge_command_queue_empty(app):
    """GET /app/commands returns 204 when queue is empty."""
    # Drain any leftover commands first
    for _ in range(10):
        try:
            resp = app.driver._get("/app/commands")
            if not resp:
                break
        except Exception:
            break
    # Now it should be empty
    resp = app.driver._get("/app/commands")
    assert resp == {}, f"Expected empty response (204), got: {resp}"


def test_bridge_post_and_get_command(app):
    """POST /app/commands then GET /app/commands returns the command."""
    cmd = {"id": "diag_cmd_1", "action": "patch", "state": {"session": {"authenticated": True}}}
    app.driver._post("/app/commands", cmd)
    resp = app.driver._get("/app/commands")
    assert resp.get("id") == "diag_cmd_1", f"Command not returned: {resp}"
    assert resp.get("action") == "patch", f"Action wrong: {resp}"


# ── 2. Bridge state cache (simulate what the app does) ───────────────────────

def test_bridge_state_cache_snake_case(app):
    """POST /app/state with snake_case keys, then GET returns them."""
    payload = {"last_command_id": "diag_cmd_2", "state": {"session": {"authenticated": True}}}
    app.driver._post("/app/state", payload)
    resp = app.driver._get("/app/state")
    # Python driver looks for last_command_id
    assert resp.get("last_command_id") == "diag_cmd_2", (
        f"snake_case key not found. Keys in response: {list(resp.keys())}. Full: {resp}"
    )


def test_bridge_state_cache_camel_case(app):
    """POST /app/state with camelCase keys, then GET — does it preserve or convert?"""
    _require_http_bridge_commands(app.driver)

    payload = {"lastCommandId": "diag_cmd_3", "state": {"session": {"authenticated": True}}}
    app.driver._post("/app/state", payload)
    resp = app.driver._get("/app/state")
    # Check both casings to find out what the bridge does
    has_snake = "last_command_id" in resp
    has_camel = "lastCommandId" in resp
    assert has_snake or has_camel, (
        f"Neither last_command_id nor lastCommandId found. Keys: {list(resp.keys())}. Full: {resp}"
    )
    if has_camel and not has_snake:
        pytest.fail(
            f"Bridge preserves camelCase keys from app — Python expects snake_case. "
            f"Keys: {list(resp.keys())}"
        )


# ── 3. App test agent polling ────────────────────────────────────────────────

def test_app_pushes_state_without_command(app):
    """The app agent should periodically push state even without a command.

    Wait up to 10s for any state to appear at GET /app/state.
    """
    _require_http_bridge_commands(app.driver)
    # Clear cached state first
    app.driver._post("/app/state", {})
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        resp = app.driver._get("/app/state")
        if resp and resp.get("state"):
            return  # Agent is alive and pushing state
        time.sleep(0.5)
    pytest.fail("App agent did not push any state within 10s — agent may not be running")


def test_app_acknowledges_noop_command(app):
    """Post a minimal patch command and check if the app acknowledges it.

    This is the core of set_state(). If this fails, the agent's poll loop
    is broken or it's not posting back the last_command_id.
    """
    _require_http_bridge_commands(app.driver)
    cmd_id = "diag_ack_test"
    app.driver._post("/app/commands", {"id": cmd_id, "action": "patch", "state": {}})

    deadline = time.monotonic() + 15
    last_resp = {}
    while time.monotonic() < deadline:
        resp = app.driver._get("/app/state")
        last_resp = resp
        # Check both casings
        acked_id = resp.get("last_command_id") or resp.get("lastCommandId")
        if acked_id == cmd_id:
            # Verify which key it came back as
            if "lastCommandId" in resp and "last_command_id" not in resp:
                pytest.fail(
                    f"App acknowledged command, but used camelCase key 'lastCommandId'. "
                    f"Python driver expects 'last_command_id'. "
                    f"Fix: TestAgent.cs should use snake_case in PushStateAsync payload."
                )
            return  # Success
        time.sleep(0.3)
    pytest.fail(
        f"App did not acknowledge command '{cmd_id}' within 15s. "
        f"Last /app/state response keys: {list(last_resp.keys())}. "
        f"last_command_id={last_resp.get('last_command_id')!r}, "
        f"lastCommandId={last_resp.get('lastCommandId')!r}. "
        f"Full response: {last_resp}"
    )


# ── 4. JSON casing in the app's state payload ───────────────────────────────

def test_app_state_field_casing(app):
    """Read whatever state the app has pushed and check key casing.

    The state protocol requires snake_case. If the app uses camelCase,
    all set_state() calls will time out because Python looks for
    last_command_id but the app sends lastCommandId.
    """
    deadline = time.monotonic() + 10
    resp = {}
    while time.monotonic() < deadline:
        resp = app.driver._get("/app/state")
        if resp:
            break
        time.sleep(0.5)

    if not resp:
        pytest.skip("No state available from app")

    top_keys = list(resp.keys())

    # Check the wrapper keys (last_command_id vs lastCommandId)
    camel_keys = [k for k in top_keys if k != k.lower() and "_" not in k and k not in ("state",)]
    if camel_keys:
        pytest.fail(
            f"App state wrapper uses camelCase keys: {camel_keys}. "
            f"Expected snake_case (last_command_id, not lastCommandId). "
            f"All top-level keys: {top_keys}"
        )

    # Check inside state dict if present
    state = resp.get("state", {})
    if isinstance(state, dict):
        state_keys = list(state.keys())
        camel_state_keys = [
            k for k in state_keys
            if any(c.isupper() for c in k) and "_" not in k
        ]
        if camel_state_keys:
            pytest.fail(
                f"App state body uses camelCase keys: {camel_state_keys}. "
                f"Expected snake_case. All state keys: {state_keys}"
            )


# ── 5. Direct HTTP round-trip (bypass Python driver) ────────────────────────

def test_raw_http_state_round_trip(app):
    """Bypass the driver and hit bridge endpoints with raw HTTP.

    Posts a command, then polls /app/state directly to see what the app returns.
    Prints the raw JSON for diagnosis.
    """
    _require_http_bridge_commands(app.driver)
    bridge_url = app.driver._url
    cmd_id = "diag_raw_http"

    # Post command
    cmd = json.dumps({"id": cmd_id, "action": "patch", "state": {}}).encode()
    req = urllib.request.Request(
        f"{bridge_url}/app/commands",
        data=cmd,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    urllib.request.urlopen(req, timeout=5)

    # Poll for state
    deadline = time.monotonic() + 15
    raw_body = ""
    while time.monotonic() < deadline:
        try:
            resp = urllib.request.urlopen(f"{bridge_url}/app/state", timeout=5)
            raw_body = resp.read().decode()
            data = json.loads(raw_body)
            if data.get("last_command_id") == cmd_id or data.get("lastCommandId") == cmd_id:
                # Report raw JSON for diagnosis
                if "lastCommandId" in raw_body:
                    pytest.fail(
                        f"Raw JSON from bridge uses camelCase: {raw_body[:500]}"
                    )
                return  # Success with snake_case
        except urllib.error.HTTPError as e:
            if e.code == 204:
                pass  # No state yet
        except Exception:
            pass
        time.sleep(0.3)

    pytest.fail(
        f"Command '{cmd_id}' not acknowledged in raw HTTP. "
        f"Last raw response: {raw_body[:500]}"
    )
