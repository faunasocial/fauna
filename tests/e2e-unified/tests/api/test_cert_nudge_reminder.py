"""When a certificate is due and no app has renewed it, the admin's closed app
gets a push reminder — once a day, and only once someone was actually told.

The witness for ``docs/features/certificates-by-themselves.md`` outcome 5, and
the rule ``docs/goal/architecture/nest/tls-certificates.md`` § C.4 (mitigation
4) and § C → *No cadence knob — and one compile-gated tick seam for mitigation
4* ratify: a client-driven-DNS nest (ACME HTTP-01 off) whose served cert is on
the floor or expiring pushes every admin a "renew your certificate" reminder,
de-duplicated to one a day by a persisted stamp that moves only when the push
reached a device.

**The tick seam.** The nudge loop ticks once at spawn — before any admin device
could subscribe — then sleeps a day. The ``test-hooks`` endpoint
``POST /api/v1/test/tls/cert-nudge/tick`` runs the SAME production tick body
(``cert_nudge::tick_once``, with the loop's own push operation) at
``now + shift_days`` and replies with what it decided. The de-dup is honoured,
never bypassed: ``shift_days`` is the one lever.

**Convention 14 — no timing anywhere.** The tick awaits its push dispatches
before the hook replies (the loop is already a background task — ``push.rs``
``maybe_send_push``), so the reply is the barrier: when it says ``dialled: 1``
the POST has reached the endpoint, and when it says ``dialled: 0`` there is no
push left in flight to arrive late. "The admin's app is closed" is observed on
the nest's own presence read (``helpers.push_dispatch.await_absent``), never
slept for.

**Why the apex is at-risk.** ``serve_tls=True`` makes the nest serve its
self-signed floor (``served_cert_spki`` is ``None`` on a plain-HTTP nest, so the
flag is load-bearing), and the claim names ``cert-nudge.example`` — a public DNS
name by ``fauna_core::resolve::is_public_dns_name``, so the live-identity-domain
gate opens — whose served cert is that floor.

**Why ``standalone_only``.** The same reason as ``test_push_dispatch.py``'s
docstring gives: the nest must dial a web-push endpoint the test process owns
on the harness's loopback, which only a locally spawned ``test-hooks`` nest
accepts and can reach; and the tick seam itself exists only in that build.

tier_3: a real nest from the locally-built binary, a real WS-RPC subscription,
a real HTTP POST over the loopback, real RFC 8291 decryption.
"""

from __future__ import annotations

import json
import urllib.request
import uuid
from pathlib import Path

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import PUSH_DIAL_S, RPC_ROUNDTRIP_S
from helpers.push_dispatch import (
    PushEndpoint,
    WebPushDevice,
    assert_one_readable_push,
    await_absent,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]

APEX = "cert-nudge.example"
#: `main.rs` logs this when it spawns the nudge loop — on every nest whose
#: ACME HTTP-01 is off, which the tick seam's own gate mirrors.
SPAWN_LOG_LINE = "At-risk TLS renewal push task started"


@pytest.fixture
def cert_nudge_nest(request, nest_mode, tmp_path_factory):
    """A dedicated TLS-serving nest claimed onto a public-looking domain: the
    persisted de-dup stamp and the admin's presence are this test's alone."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "cert-nudge",
        serve_tls=True, claim_domain=APEX,
    )
    yield nest
    cleanup()


@pytest.mark.feature("certificates-by-themselves")
def test_the_admin_is_reminded_once_a_day_and_only_once_told(cert_nudge_nest):
    nest = cert_nudge_nest
    url = nest["url"]
    admin_sk = _as_signing_key(nest["admin"]["signing_key"])
    admin_id = bytes(admin_sk.verify_key)

    log = Path(nest["log_path"]).read_text(errors="replace")
    assert SPAWN_LOG_LINE in log, (
        "the nudge loop must be running on this nest (ACME HTTP-01 off, a "
        f"served-cert resolver, a push service); {SPAWN_LOG_LINE!r} is not in "
        f"{nest['log_path']}"
    )

    # ── (a) No admin device yet: the tick finds the apex at risk, reaches
    # nobody, and burns no day (the de-dup records attention given).
    first = _tick(url, 0)
    assert first["apex"] == APEX, f"the domain gate must open on {APEX}: {first!r}"
    assert APEX in first["at_risk"], f"the floor-served apex is at risk: {first!r}"
    assert (first["nudged"], first["dialled"]) == (False, 0), (
        f"with no admin device nothing is dialled and nothing recorded: {first!r}"
    )

    device = WebPushDevice()
    with PushEndpoint() as endpoint:
        # ── (b) The admin's app subscribes this device for push, then closes.
        with WsRpcAdminClient(url, actor_id=admin_id, signing_key=bytes(admin_sk)) as app:
            vapid_public_key = app.call("fauna.push.vapid_key", {})["public_key"]
            app.call(
                "fauna.push.subscribe",
                {
                    "device_id": f"cert-nudge-{uuid.uuid4().hex[:8]}",
                    "endpoint": endpoint.url,
                    "key_p256dh": device.p256dh_b64,
                    "key_auth": device.auth_b64,
                    "transport": "web-push",
                },
            )
        await_absent(url, admin_id.hex(), RPC_ROUNDTRIP_S)
        assert not endpoint.has_new(), "nothing pushed before the tick"

        second = _tick(url, 0)
        assert (second["nudged"], second["dialled"]) == (True, 1), (
            "the tick after the device-less one must still remind the admin — "
            f"the device-less tick burned no day: {second!r}"
        )
        assert APEX in second["at_risk"], second
        assert_one_readable_push(
            endpoint, device, vapid_public_key, trigger="the at-risk renewal reminder"
        )

        # ── (c) Again the same day: the persisted de-dup holds it back.
        third = _tick(url, 0)
        assert (third["nudged"], third["dialled"]) == (False, 0), (
            f"a second tick inside the day must not re-nudge: {third!r}"
        )
        # The reply is the barrier — the tick awaits its dispatches — so a
        # POST that is not here now is not coming.
        assert not endpoint.has_new(), "the de-duplicated tick pushed anyway"

        # ── (d) A day on, still unrenewed: reminded again.
        fourth = _tick(url, 1)
        assert (fourth["nudged"], fourth["dialled"]) == (True, 1), (
            f"a day after the last reminder an at-risk cert nudges again: {fourth!r}"
        )
        assert_one_readable_push(
            endpoint, device, vapid_public_key, trigger="the next day's reminder"
        )


def _tick(url: str, shift_days: int) -> dict:
    """One whole nudge tick at ``now + shift_days`` through the ``test-hooks``
    seam; returns ``{"apex", "at_risk", "nudged", "dialled"}``. Bounded by the
    push-dial budget: the tick awaits its dispatches before replying."""
    req = urllib.request.Request(
        f"{url}/api/v1/test/tls/cert-nudge/tick",
        data=json.dumps({"shift_days": shift_days}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=PUSH_DIAL_S) as resp:
        assert resp.status == 200, f"cert-nudge tick hook returned {resp.status}"
        return json.loads(resp.read())


def _as_signing_key(raw) -> SigningKey:
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))
