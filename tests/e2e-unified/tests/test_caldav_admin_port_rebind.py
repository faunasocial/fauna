"""tier_3: an admin-set CalDAV port change must MOVE the live CalDAV listener.

The user's request (2026-06-19): *change the CalDAV port using only the client →
confirm a calendar round-trip survives on the new port.* This is the acceptance
test for the admin-settable CalDAV port (caldav-server.md § Network exposure — the
port is an admin **choice** backed by nest state, default 8443, not a config-file
knob; tracked internally, § FOLLOW-UP item 5).

The whole admin→nest-state→MDA-bind→rebind chain, end to end on real binaries:

    admin `fauna.bridges.set_caldav_port`            (Admin RPC → nest state)
        → nest `fetch_config.caldav_port` projection
        → Go MDA `resolveMDAListenAddrs` (no hatch → `:<caldav_port>`)
        → CalDAV listener binds the admin port
        → admin CHANGES the port → nest `config_changed`
        → MDA `caldavPortRebindNeeded` (armed by `CalDAVBindIsAdminPort`) exits 0
        → s6 restarts it (here: the test plays supervisor via `mda.respawn()`)
        → listener re-binds the NEW port, the old port goes dead.

Only tier_3 catches this: it is a cross-binary contract (the Rust nest's
`caldav_port` projection ↔ the Go MDA's bind + rebind logic) over the real config
WS-RPC. A stub or pure-Rust test can't exercise the Go bind/rebind path, and the
existing any-locator matrix PINS the CalDAV port via the operator-hatch — so the
admin `caldav_port` is never the authority there. The `dedicated_caldav_admin_port_nest`
fixture spawns the MDA WITHOUT a `caldav_listen_https` hatch so the admin port
governs and the rebind exit is armed (`CalDAVBindIsAdminPort`).

Production faithfulness: a real deployment has an s6 supervisor restart the MDA on
its rebind exit; the binaries e2e has none, so `handle.mda.respawn()` re-launches
the IDENTICAL process (same keyfile → reconnects as the already-approved bridge,
same hatch-free shape → re-reads the new `caldav_port`). The "change the port from
the client" half is the Admin WS-RPC the client UI will call; this test drives that
RPC directly (the per-app UI field is a separate, entrusted lift).

Test taxonomy: `tier_3` (mocking depth) — real `fauna-nest` + real `fauna-mail-bridge`
MDA binaries, real CalDAV HTTPS wire, real client-sealed calendar store.
"""

from __future__ import annotations

import time

import pytest
import requests
import urllib3
from requests.auth import HTTPBasicAuth

from clients.ws_rpc_admin_client import WsRpcAdminClient
from drivers.port_util import find_free_port
from helpers.caldav_client import CalDAVClient, build_vevent
from helpers.mail_dedicated_nest import (
    alias_admin_to_address,
    dedicated_node_url,
    login_as_nest_admin,
    mint_caldav_mailbox,
    require_caldav_mailbox_mint_supported,
)
from helpers.scheduling_inbox import utc_offset as _utc

pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.tui,
]

# The PLAIN CalDAV credential the client mints and the MUA authenticates with.
_PASSWORD = "CalDAVAdminPortRebindPlainPw01Ab"


def _set_caldav_port(nest, port: int) -> None:
    """Change the admin CalDAV port over the Admin-class `fauna.bridges.set_caldav_port`
    WS-RPC — the exact kind the client admin UI will call. `actor_id` is the
    admin's raw 32-byte verify key (CBOR byte string), mirroring
    `alias_admin_to_address`."""
    admin = nest["admin"]
    admin_ws = WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with admin_ws:
        admin_ws.call("fauna.bridges.set_caldav_port", {"port": port})


def _assert_port_dead(port: int, *, timeout: float = 20.0) -> None:
    """Poll until a TLS handshake to `127.0.0.1:port` is REFUSED — the old listener
    is gone after the rebind. A `ConnectionError` (refused / reset) is the success
    signal; any HTTP/TLS response means something is still serving there."""
    urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
    url = f"https://127.0.0.1:{port}/caldav/"
    deadline = time.monotonic() + timeout
    last = "(never attempted)"
    while time.monotonic() < deadline:
        try:
            r = requests.request("PROPFIND", url, timeout=5.0, verify=False)
            last = f"still serving: HTTP {r.status_code}"
        except requests.exceptions.ConnectionError:
            return  # refused — the old listener is gone
        except requests.exceptions.RequestException as e:
            last = type(e).__name__
        time.sleep(1.0)
    pytest.fail(
        f"old CalDAV port {port} was still reachable {timeout:.0f}s after the rebind "
        f"({last}) — the listener did not move off the old port"
    )


def _tail_mda_log(mda) -> str:
    """The MDA's CalDAV/bind/rebind log lines — makes an MDA-exit/rebind failure
    self-diagnosing (the config watcher logs `caldav_port_now` on rebind)."""
    path = getattr(mda, "log_file_path", None)
    if not path:
        return "  (mda log path not exposed)"
    try:
        with open(path, "r", errors="replace") as fh:
            lines = [
                ln.rstrip()
                for ln in fh
                if any(
                    k in ln.lower()
                    for k in ("caldav", "rebind", "config_changed", "listen", "bind",
                              "exit", "port")
                )
            ]
        return "\n".join("    " + ln for ln in lines[-30:]) or "    (no relevant lines)"
    except Exception as e:  # noqa: BLE001 — diagnostic, never mask the real assert
        return f"  (could not read {path}: {e!r})"


@pytest.mark.feature("admin-calendar-contacts-files")
def test_admin_caldav_port_change_rebinds_listener(
    app, dedicated_caldav_admin_port_nest, request
):
    """An admin changes the CalDAV port over WS-RPC → the MDA rebinds → a calendar
    round-trip survives on the NEW port (event written on the old port is still
    visible) and the OLD port goes dead. The headless, on-principle proof of the
    user's "change the port from the client and the round-trip survives" ask."""
    require_caldav_mailbox_mint_supported(app.driver)

    handle = dedicated_caldav_admin_port_nest
    nest = handle.nest
    domain = handle.domain  # fauna.test
    mda = handle.mda
    port_a = handle.caldav_port  # the admin port the MDA bound at boot
    assert mda is not None and mda.proc.poll() is None, (
        f"MDA must be alive on the admin port at start (see {_tail_mda_log(mda)})"
    )

    # 1) Log the linux app in as the nest admin + mint the CalDAV mailbox (the
    #    shared MSEK + the `default` credential the raw CalDAV client AUTHs with +
    #    the canonical `admin@<domain>` alias). Email stays off (CalDAV-only nest).
    login_as_nest_admin(app, nest, dedicated_node_url(app, handle, request))
    mint_caldav_mailbox(app.driver, password=_PASSWORD)
    addr = alias_admin_to_address(nest, domain)  # admin@fauna.test

    # 2) Round-trip an event on the original admin port A.
    base_a = f"https://127.0.0.1:{port_a}"
    mua_a = CalDAVClient(base_a, addr, _PASSWORD, verify=False)
    mua_a.wait_until_serving(timeout=120.0)
    cal = mua_a.personal_calendar()
    nonce = f"adminport{int(time.time())}"
    uid_a = f"{nonce}-a@{domain}"
    summary_a = f"Event-A-{nonce}"
    mua_a.put_event(cal, uid_a, build_vevent(uid_a, summary_a, _utc(60), _utc(120)))
    assert summary_a in mua_a.summaries(cal), (
        f"event A must be visible on the admin port {port_a} BEFORE the change"
    )

    # 3) The admin CHANGES the CalDAV port A → B over WS-RPC (the client-UI path).
    port_b = find_free_port()
    assert port_b != port_a, "the new port must differ from the old"
    _set_caldav_port(nest, port_b)

    # 4) The MDA's config watcher exits 0 on the port change (it expects s6 to
    #    restart it). The test plays supervisor: wait for the exit, then respawn.
    try:
        mda.proc.wait(timeout=45)
    except Exception:  # subprocess.TimeoutExpired
        pytest.fail(
            "MDA did not exit after set_caldav_port — the config-driven rebind exit "
            "(caldavPortRebindNeeded, armed by CalDAVBindIsAdminPort) did not fire.\n"
            f"MDA log (tail):\n{_tail_mda_log(mda)}"
        )
    assert mda.proc.returncode == 0, (
        f"MDA must exit 0 on a clean rebind, got returncode={mda.proc.returncode}.\n"
        f"MDA log (tail):\n{_tail_mda_log(mda)}"
    )
    mda.respawn(timeout=45.0)

    # 5) The old port A is dead; port B serves; event A SURVIVED the change and a
    #    fresh write on B works — the round-trip is intact on the new port.
    _assert_port_dead(port_a)
    base_b = f"https://127.0.0.1:{port_b}"
    mua_b = CalDAVClient(base_b, addr, _PASSWORD, verify=False)
    mua_b.wait_until_serving(timeout=120.0)
    cal_b = mua_b.personal_calendar()

    deadline = time.monotonic() + 30.0
    seen: list[str] = []
    while time.monotonic() < deadline:
        seen = mua_b.summaries(cal_b)
        if summary_a in seen:
            break
        time.sleep(2.0)
    assert summary_a in seen, (
        f"event A (written on the old port {port_a}) must SURVIVE the port change "
        f"and be visible on the new port {port_b}; saw {seen!r}.\n"
        f"MDA log (tail):\n{_tail_mda_log(mda)}"
    )

    uid_b = f"{nonce}-b@{domain}"
    summary_b = f"Event-B-{nonce}"
    mua_b.put_event(cal_b, uid_b, build_vevent(uid_b, summary_b, _utc(180), _utc(240)))
    sums = mua_b.summaries(cal_b)
    assert summary_b in sums, (
        f"a new write on the new port {port_b} must work; saw {sums!r}"
    )
    assert summary_a in sums, (
        f"event A must remain visible alongside B on the new port; saw {sums!r}"
    )
