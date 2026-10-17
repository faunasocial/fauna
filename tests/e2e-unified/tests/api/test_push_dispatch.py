"""With the app closed, a new message, knock or invite reaches the device as a
push — and with the app connected, it does not.

The witness for ``docs/features/notifications.md`` outcome 8, and the rule
``docs/goal/architecture/apps/common.md`` § Push Notifications / § Dispatch
Logic ratifies: the nest pushes on inbox delivery, knocks and group invites,
and decides **per device** (ruled 2026-09-26) — a ``web-push``/``apns`` row is
skipped while a live connection announcing its ``device_id``
(``fauna.push.presence``) exists and dialled otherwise; a live connection that
announced nothing keeps the old actor-wide skip; a ``ws-device`` row is
delivered as a ``fauna.push.notification`` frame over the device's own
connection and skipped when none is live. A wake stand-in's announce
(``stand_in: true``, the sync agent's; ruled 2026-10-03) delivers and never
suppresses: it receives its device's ``ws-device`` frame, and its device's
relay rows are still dialled. Each web-push payload is
RFC 8291 ``aes128gcm``-encrypted to the subscription's own P-256 key, so a
relay in between cannot read it.

The subscribe/unsubscribe/key-generation tests in ``test_push_api.py`` stop at
the registration; nothing observed a DISPATCH until this file.

**The device is the test.** It subscribes a web-push endpoint it owns — a
loopback HTTP listener standing in for the browser vendor's relay — with a real
P-256 keypair and auth secret, exactly what a browser's ``PushManager`` hands
the app. A push "reaches the device" when the nest POSTs to that endpoint a
body the device can decrypt with its private key, signed with the VAPID key the
nest publishes. Decrypting it is the point: a POST whose body the subscriber
could not open would not reach anyone.

**Why the senders are headless (e2e rule 8b).** This is a ``[nest]`` outcome:
what is under test is the nest's dispatch decision and its wire, not an app's
gesture. The three triggers ride the production kinds a second person's app
would issue (``fauna.inbox.send``, ``fauna.conversations.welcome.deliver``).

**Convention 14 — no timing anywhere.** Three causal facts carry every
assertion:

1. **The decision is synchronous; the dial is not.** A sender must never run at
   the speed of the recipient's push relay, so the nest decides *whether* to
   push inside the sender's RPC handler, before the reply, and hands the POST
   to a background task (``push.rs`` ``dispatch_offline_push``). When the
   sender's call returns, the decision is made and the POST may not have
   happened yet. So:

   * the decision is read off the nest's test-hooks counters
     (``GET /api/v1/test/push/dispatches``): ``initiated`` is bumped inside
     the sender's handler, so the reply is the barrier for "exactly one
     dispatch was decided". Which rows it dials or delivers needs the
     subscription read, which is background work, so the task sums its
     outcome into ``dialled`` / ``delivered`` and only *then* bumps
     ``settled``. The test waits for ``settled`` to reach ``initiated`` — a
     positive event — after which the sums are final.
   * the **negative** ("present → no dial") is an unchanged ``dialled`` after
     that settle, never an empty listener: a late push reads identical to no
     push, but a settled dispatch that dialled nothing has no push left in it.
   * the **positive** also awaits the listener's own receipt of the POST, or
     the device's receipt of the frame, under a named budget that a green run
     never spends.
2. **"Offline" is observed, not slept for.** The nest drops an actor's
   connection from its registry after the socket's serve loop returns, a
   moment after the client closes. On this dedicated nest every connection is
   one the test opened, so ``fauna.admin.stats``'s ``ws_connections`` falling
   back to the admin-only baseline is proof the recipient holds none.
   "Online" needs no wait at all: a reply on the recipient's own connection
   means the nest registered it before serving the call.

**Why ``standalone_only``.** The nest must dial an endpoint the test process
owns, on the harness's loopback, and a nest only does that in a ``test-hooks``
build: a shipped nest refuses any web-push endpoint that is not https on a
public address, at subscribe and again at every dial (``common.md``
§ Registration / § Dispatch Logic; pinned by ``push.rs``'s own tests and by
``test_push_api.py``'s refusal test). The test-hooks allowance is a loopback
IP literal and nothing wider. Only the locally spawned nest is such a build
*and* shares this loopback; a docker nest's ``127.0.0.1`` is its own container
and a live nest's is a remote box. The dependency is the subject of the test
(where the push goes), not a limit it spends, so it is declared here rather
than inferred.

tier_3: a real nest from the locally-built binary, real WS-RPC senders, a real
HTTP POST over the loopback, real RFC 8291 decryption.
"""

from __future__ import annotations

import os
import uuid

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from helpers.budgets import PUSH_DIAL_S, RPC_ROUNDTRIP_S
from helpers.push_dispatch import (
    PushEndpoint,
    WebPushDevice,
    actor_client,
    assert_one_readable_push,
    await_offline,
    await_settled,
    decided as dispatches_decided,
    send_inbox_mail,
    ws_connections,
)
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]


@pytest.fixture
def push_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest: the offline observation reads a nest-wide
    connection count, which is exact only where every connection is this
    test's own."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "push-dispatch")
    yield nest
    cleanup()


@pytest.mark.feature("notifications")
def test_push_reaches_an_offline_device_for_a_knock_a_message_and_an_invite(push_nest):
    nest = push_nest
    port, url = nest["port"], nest["url"]
    admin_sk = _as_signing_key(nest["admin"]["signing_key"])

    recipient = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
    device = WebPushDevice()

    with PushEndpoint() as endpoint, _client(url, bytes(admin_sk.verify_key), admin_sk) as admin:
        baseline = ws_connections(admin)

        # ── The device registers for push, then the app closes.
        with actor_client(url, recipient) as rc:
            vapid_public_key = rc.call("fauna.push.vapid_key", {})["public_key"]
            rc.call(
                "fauna.push.subscribe",
                {
                    "device_id": f"push-dispatch-{uuid.uuid4().hex[:8]}",
                    "endpoint": endpoint.url,
                    "key_p256dh": device.p256dh_b64,
                    "key_auth": device.auth_b64,
                    "transport": "web-push",
                },
            )
        await_offline(admin, baseline)

        # ── 1. A knock: a stranger writes under the default `allow_knock`
        # inbox mode, so the nest holds it as a knock.
        stranger = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
        decided = dispatches_decided(url)
        reply = send_inbox_mail(url, stranger, recipient, "knock")
        assert reply.get("inbox_id") is None, (
            f"a stranger's first mail under allow_knock must be held as a knock: {reply!r}"
        )
        decided = _assert_one_readable_push(
            url, decided, endpoint, device, vapid_public_key, trigger="a knock"
        )

        # ── 2. A message: the recipient opens their inbox (the app is briefly
        # open for that), closes again, and a new stranger's mail delivers.
        with actor_client(url, recipient) as rc:
            rc.call("fauna.inbox.mode.set", {"mode": "open"})
        await_offline(admin, baseline)
        writer = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
        reply = send_inbox_mail(url, writer, recipient, "message")
        assert reply.get("inbox_id") is not None, (
            f"under the open inbox mode the mail must deliver to an inbox row: {reply!r}"
        )
        decided = _assert_one_readable_push(
            url, decided, endpoint, device, vapid_public_key, trigger="a new message"
        )

        # ── 3. An invite: someone delivers a conversation Welcome.
        inviter = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
        with actor_client(url, inviter) as ic:
            ic.call(
                "fauna.conversations.welcome.deliver",
                {
                    "recipient_actor_id": recipient["actor_id_hex"],
                    "channel_id": os.urandom(32).hex(),
                    "welcome_bytes": os.urandom(32),
                    "kind": {"type": "dm"},
                },
            )
        decided = _assert_one_readable_push(
            url, decided, endpoint, device, vapid_public_key, trigger="an invite"
        )

        # ── 4. An app that announces no device is open (every current app today): the
        # compatibility rider keeps the old actor-wide skip for relay rows.
        with actor_client(url, recipient) as rc:
            # A reply on the recipient's own connection is the causal barrier
            # for "connected": the nest registers a connection before it
            # serves any call on it.
            rc.call("fauna.inbox.mode.get", {})
            late_writer = create_actor_and_register(
                port, base_url=url, admin_signing_key=admin_sk
            )
            before = await_settled(url)
            reply = send_inbox_mail(url, late_writer, recipient, "while connected")
            assert reply.get("inbox_id") is not None, f"the mail must still deliver: {reply!r}"
            after = await_settled(url, initiated=before["initiated"] + 1)
            assert after["dialled"] == before["dialled"], (
                "a live connection that announced no device must suppress every "
                "web-push row of the actor (common.md § Dispatch Logic, the "
                f"compatibility rider); the nest dialled {after['dialled'] - before['dialled']}"
            )


@pytest.mark.feature("notifications")
def test_push_is_decided_per_device(push_nest):
    """The 2026-09-26 per-device rule: one device's open app quiets only that
    device; an unannounced connection quiets every relay row; a ``ws-device``
    row is delivered over the device's own connection, and skipped when that
    connection is gone."""
    nest = push_nest
    port, url = nest["port"], nest["url"]
    admin_sk = _as_signing_key(nest["admin"]["signing_key"])

    recipient = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
    inviter = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
    tag = uuid.uuid4().hex[:8]
    phone, browser, desk = f"phone-{tag}", f"browser-{tag}", f"desk-{tag}"
    phone_device, browser_device = WebPushDevice(), WebPushDevice()

    with (
        PushEndpoint() as phone_endpoint,
        PushEndpoint() as browser_endpoint,
        _client(url, bytes(admin_sk.verify_key), admin_sk) as admin,
        actor_client(url, recipient) as phone_app,
    ):
        # The trigger is a stranger's conversation Welcome, which only an
        # open inbox accepts.
        phone_app.call("fauna.inbox.mode.set", {"mode": "open"})
        for device_id, endpoint, device in (
            (phone, phone_endpoint, phone_device),
            (browser, browser_endpoint, browser_device),
        ):
            phone_app.call(
                "fauna.push.subscribe",
                {
                    "device_id": device_id,
                    "endpoint": endpoint.url,
                    "key_p256dh": device.p256dh_b64,
                    "key_auth": device.auth_b64,
                    "transport": "web-push",
                },
            )

        # ── 1. The phone's app is open and says so; the browser is closed.
        _announce(phone_app, phone)
        counters = _invite(url, inviter, recipient, await_settled(url))
        assert counters["dialled_delta"] == 1, (
            "with only the phone present, exactly the browser's row must be "
            f"dialled (common.md § Dispatch Logic); dialled {counters['dialled_delta']}"
        )
        wait_until(
            browser_endpoint.has_new,
            PUSH_DIAL_S,
            diagnose=lambda: "the absent browser's row was dialled but no POST reached it",
        )
        assert len(browser_endpoint.take_new()) == 1
        assert not phone_endpoint.has_new(), "the present phone must not be pushed"

        # ── 2. An app announcing nothing opens too: every relay row
        # is quiet, the absent browser's included.
        base = ws_connections(admin)
        with actor_client(url, recipient) as old_app:
            old_app.call("fauna.inbox.mode.get", {})
            _announce(phone_app, phone)
            counters = _invite(url, inviter, recipient, await_settled(url))
            assert counters["dialled_delta"] == 0, (
                "an unannounced live connection must suppress every web-push "
                "row of the actor (common.md § Dispatch Logic, the "
                f"compatibility rider); dialled {counters['dialled_delta']}"
            )
        _await_connections(admin, base)

        # ── 3. A desktop's agent announces its device and holds a ws-device
        # row: the push rides its own connection as a frame.
        with actor_client(url, recipient) as desk_agent:
            desk_agent.call(
                "fauna.push.subscribe",
                {"device_id": desk, "endpoint": desk, "transport": "ws-device"},
            )
            _announce(desk_agent, desk)
            _announce(phone_app, phone)
            counters = _invite(url, inviter, recipient, await_settled(url))
            assert counters["delivered_delta"] == 1, (
                "a present ws-device row must be delivered over its own "
                f"connection; delivered {counters['delivered_delta']}"
            )
            frame = desk_agent.wait_for_push("fauna.push.notification", timeout=PUSH_DIAL_S)
            assert frame.payload.get("title") and frame.payload.get("body"), frame.payload
            assert frame.payload.get("url", "").startswith("/"), (
                f"the frame carries the deep link a tap opens: {frame.payload!r}"
            )
            browser_endpoint.take_new()  # the still-absent browser was dialled too
        _await_connections(admin, base)

        # ── 4. The desktop is off: its row is skipped and nothing is queued.
        _announce(phone_app, phone)
        counters = _invite(url, inviter, recipient, await_settled(url))
        assert counters["delivered_delta"] == 0, (
            "a ws-device row with no live connection announcing it must be "
            f"skipped (common.md § Dispatch Logic); delivered {counters['delivered_delta']}"
        )


@pytest.mark.feature("notifications")
def test_a_stand_in_announce_delivers_and_never_suppresses(push_nest):
    """The 2026-10-03 stand-in rule: a connection announcing a device with
    ``stand_in: true`` — the sync agent, which renders nothing itself — is a
    delivery point for that device's ``ws-device`` row and nothing else. A
    device holds one row (keyed by its ``device_id``), so the two halves ride
    two devices, as they do in the field: a Mac whose relay row (``apns``
    there, a ``web-push`` row on the loopback here) is still dialled while only
    its agent announces it, until its app's plain announce quiets it; and a
    linux desk whose ``ws-device`` row reaches its agent's stand-in connection
    (common.md § Dispatch Logic)."""
    nest = push_nest
    port, url = nest["port"], nest["url"]
    admin_sk = _as_signing_key(nest["admin"]["signing_key"])

    recipient = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
    inviter = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
    tag = uuid.uuid4().hex[:8]
    mac, desk, phone = f"mac-{tag}", f"desk-{tag}", f"phone-{tag}"
    mac_device = WebPushDevice()

    with (
        PushEndpoint() as mac_endpoint,
        _client(url, bytes(admin_sk.verify_key), admin_sk) as admin,
        actor_client(url, recipient) as phone_app,
        actor_client(url, recipient) as mac_agent,
        actor_client(url, recipient) as desk_agent,
    ):
        phone_app.call("fauna.inbox.mode.set", {"mode": "open"})
        phone_app.call(
            "fauna.push.subscribe",
            {
                "device_id": mac,
                "endpoint": mac_endpoint.url,
                "key_p256dh": mac_device.p256dh_b64,
                "key_auth": mac_device.auth_b64,
                "transport": "web-push",
            },
        )
        desk_agent.call(
            "fauna.push.subscribe",
            {"device_id": desk, "endpoint": desk, "transport": "ws-device"},
        )

        # ── 1. Both apps are closed; each machine's agent announces its device
        # as a stand-in. The Mac's relay row is dialled AND the desk's
        # ws-device frame reaches its agent. The phone's app announcing its own
        # device keeps the compatibility rider out of the picture.
        _announce(phone_app, phone)
        _announce(mac_agent, mac, stand_in=True)
        _announce(desk_agent, desk, stand_in=True)
        counters = _invite(url, inviter, recipient, await_settled(url))
        assert counters["dialled_delta"] == 1, (
            "a stand-in announce must never suppress its device's relay row "
            f"(common.md § Dispatch Logic); dialled {counters['dialled_delta']}"
        )
        assert counters["delivered_delta"] == 1, (
            "a stand-in announce is the delivery point of its device's ws-device "
            f"row; delivered {counters['delivered_delta']}"
        )
        wait_until(
            mac_endpoint.has_new,
            PUSH_DIAL_S,
            diagnose=lambda: "the stand-in's device row was dialled but no POST reached it",
        )
        assert len(mac_endpoint.take_new()) == 1
        frame = desk_agent.wait_for_push("fauna.push.notification", timeout=PUSH_DIAL_S)
        assert frame.payload.get("title") and frame.payload.get("body"), frame.payload

        # ── 2. The Mac's app opens and announces its device plainly: the relay
        # row is quiet now; the desk's ws-device row still delivers.
        base = ws_connections(admin)
        with actor_client(url, recipient) as mac_app:
            _announce(mac_app, mac)
            _announce(mac_agent, mac, stand_in=True)
            _announce(desk_agent, desk, stand_in=True)
            _announce(phone_app, phone)
            counters = _invite(url, inviter, recipient, await_settled(url))
            assert counters["dialled_delta"] == 0, (
                "an app's plain announce of the device must still skip its "
                f"relay row; dialled {counters['dialled_delta']}"
            )
            assert counters["delivered_delta"] == 1, (
                f"the ws-device row still delivers; delivered {counters['delivered_delta']}"
            )
        _await_connections(admin, base)


def _announce(client: WsRpcAdminClient, device_id: str, *, stand_in: bool = False) -> None:
    """``fauna.push.presence`` on ``client``'s connection — an app's plain
    announce, or a wake stand-in's (``stand_in: true``). Re-sent right before
    each trigger: the client replaces a socket parked past its idle limit, and
    a fresh socket has announced nothing — the same reason the shared client
    announces on every (re)connect. The reply is the barrier: the tag is set
    before the nest answers."""
    request = {"device_id": device_id}
    if stand_in:
        request["stand_in"] = True
    assert client.call("fauna.push.presence", request) == {"ok": True}


def _invite(url: str, inviter: dict, recipient: dict, before: dict) -> dict:
    """Trigger one push to ``recipient`` (a conversation Welcome), wait for its
    dispatch to settle, and return the counters with the deltas it caused."""
    with actor_client(url, inviter) as ic:
        ic.call(
            "fauna.conversations.welcome.deliver",
            {
                "recipient_actor_id": recipient["actor_id_hex"],
                "channel_id": os.urandom(32).hex(),
                "welcome_bytes": os.urandom(32),
                "kind": {"type": "dm"},
            },
        )
    after = await_settled(url, initiated=before["initiated"] + 1)
    after["dialled_delta"] = after["dialled"] - before["dialled"]
    after["delivered_delta"] = after["delivered"] - before["delivered"]
    return after


def _assert_one_readable_push(
    url: str, decided_before: int, endpoint, device, vapid_public_key: str, *, trigger: str
) -> int:
    """``trigger`` made the nest decide exactly one dispatch, its POST arrived,
    and the device can read it. Returns the new decision count.

    Called after the sender's call returned, so the decision count is final for
    this trigger (docstring fact 1); only the POST itself is awaited.
    """
    decided = dispatches_decided(url)
    assert decided == decided_before + 1, (
        f"{trigger} for an actor with no live connection must make the nest "
        "decide exactly one push dispatch before it replies to the sender "
        f"(common.md § Dispatch Logic); it decided {decided - decided_before}"
    )
    assert_one_readable_push(endpoint, device, vapid_public_key, trigger=trigger)
    return decided


# ── wire helpers ──────────────────────────────────────────────────────────────


def _await_connections(admin: WsRpcAdminClient, count: int) -> None:
    """The nest-wide connection count is back to ``count``: a closed client's
    connection has left the registry (the dedicated nest's connections are all
    this test's own)."""
    wait_until(
        lambda: ws_connections(admin) == count,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the nest still counts {ws_connections(admin)} live connection(s), "
            f"expected {count}: a closed client's connection never left the registry"
        ),
    )


def _client(url: str, actor_id: bytes, signing_key: SigningKey) -> WsRpcAdminClient:
    return WsRpcAdminClient(url, actor_id=actor_id, signing_key=bytes(signing_key))


def _as_signing_key(raw) -> SigningKey:
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))
