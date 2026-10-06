"""With the app closed, a new message, knock or invite reaches the device as a
push — and with the app connected, it does not.

The witness for ``docs/features/notifications.md`` outcome 8, and the rule
``docs/goal/architecture/apps/common.md`` § Push Notifications / § Dispatch
Logic ratifies: the nest pushes on inbox delivery, knocks and group invites,
and decides **per device** (ruled 2026-09-26) — a ``web-push``/``apns`` row is
skipped while a live connection announcing its ``device_id``
(``fauna.push.presence``) exists and dialled otherwise; a live connection that
announced nothing (every current app, until one sends ``fauna.push.presence``) keeps the old actor-wide skip; a
``ws-device`` row is delivered as a ``fauna.push.notification`` frame over the
device's own connection and skipped when none is live. Each web-push payload is
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

import base64
import json
import os
import threading
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.asymmetric.utils import encode_dss_signature
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import build_email_inbox_payload, create_actor_and_register
from helpers.budgets import PUSH_DIAL_S, RPC_ROUNDTRIP_S
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
    device = _Device()

    with _PushEndpoint() as endpoint, _client(url, bytes(admin_sk.verify_key), admin_sk) as admin:
        baseline = _ws_connections(admin)

        # ── The device registers for push, then the app closes.
        with _actor_client(url, recipient) as rc:
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
        _await_offline(admin, baseline)

        # ── 1. A knock: a stranger writes under the default `allow_knock`
        # inbox mode, so the nest holds it as a knock.
        stranger = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
        decided = _dispatches_decided(url)
        reply = _send_email(url, stranger, recipient, "knock")
        assert reply.get("inbox_id") is None, (
            f"a stranger's first mail under allow_knock must be held as a knock: {reply!r}"
        )
        decided = _assert_one_readable_push(
            url, decided, endpoint, device, vapid_public_key, trigger="a knock"
        )

        # ── 2. A message: the recipient opens their inbox (the app is briefly
        # open for that), closes again, and a new stranger's mail delivers.
        with _actor_client(url, recipient) as rc:
            rc.call("fauna.inbox.mode.set", {"mode": "open"})
        _await_offline(admin, baseline)
        writer = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
        reply = _send_email(url, writer, recipient, "message")
        assert reply.get("inbox_id") is not None, (
            f"under the open inbox mode the mail must deliver to an inbox row: {reply!r}"
        )
        decided = _assert_one_readable_push(
            url, decided, endpoint, device, vapid_public_key, trigger="a new message"
        )

        # ── 3. An invite: someone delivers a conversation Welcome.
        inviter = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
        with _actor_client(url, inviter) as ic:
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
        with _actor_client(url, recipient) as rc:
            # A reply on the recipient's own connection is the causal barrier
            # for "connected": the nest registers a connection before it
            # serves any call on it.
            rc.call("fauna.inbox.mode.get", {})
            late_writer = create_actor_and_register(
                port, base_url=url, admin_signing_key=admin_sk
            )
            before = _await_settled(url)
            reply = _send_email(url, late_writer, recipient, "while connected")
            assert reply.get("inbox_id") is not None, f"the mail must still deliver: {reply!r}"
            after = _await_settled(url, initiated=before["initiated"] + 1)
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
    phone_device, browser_device = _Device(), _Device()

    with (
        _PushEndpoint() as phone_endpoint,
        _PushEndpoint() as browser_endpoint,
        _client(url, bytes(admin_sk.verify_key), admin_sk) as admin,
        _actor_client(url, recipient) as phone_app,
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
        counters = _invite(url, inviter, recipient, _await_settled(url))
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
        base = _ws_connections(admin)
        with _actor_client(url, recipient) as old_app:
            old_app.call("fauna.inbox.mode.get", {})
            _announce(phone_app, phone)
            counters = _invite(url, inviter, recipient, _await_settled(url))
            assert counters["dialled_delta"] == 0, (
                "an unannounced live connection must suppress every web-push "
                "row of the actor (common.md § Dispatch Logic, the "
                f"compatibility rider); dialled {counters['dialled_delta']}"
            )
        _await_connections(admin, base)

        # ── 3. A desktop's agent announces its device and holds a ws-device
        # row: the push rides its own connection as a frame.
        with _actor_client(url, recipient) as desk_agent:
            desk_agent.call(
                "fauna.push.subscribe",
                {"device_id": desk, "endpoint": desk, "transport": "ws-device"},
            )
            _announce(desk_agent, desk)
            _announce(phone_app, phone)
            counters = _invite(url, inviter, recipient, _await_settled(url))
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
        counters = _invite(url, inviter, recipient, _await_settled(url))
        assert counters["delivered_delta"] == 0, (
            "a ws-device row with no live connection announcing it must be "
            f"skipped (common.md § Dispatch Logic); delivered {counters['delivered_delta']}"
        )


def _announce(client: WsRpcAdminClient, device_id: str) -> None:
    """``fauna.push.presence`` on ``client``'s connection. Re-sent right before
    each trigger: the client replaces a socket parked past its idle limit, and
    a fresh socket has announced nothing — the same reason the shared client
    announces on every (re)connect. The reply is the barrier: the tag is set
    before the nest answers."""
    assert client.call("fauna.push.presence", {"device_id": device_id}) == {"ok": True}


def _invite(url: str, inviter: dict, recipient: dict, before: dict) -> dict:
    """Trigger one push to ``recipient`` (a conversation Welcome), wait for its
    dispatch to settle, and return the counters with the deltas it caused."""
    with _actor_client(url, inviter) as ic:
        ic.call(
            "fauna.conversations.welcome.deliver",
            {
                "recipient_actor_id": recipient["actor_id_hex"],
                "channel_id": os.urandom(32).hex(),
                "welcome_bytes": os.urandom(32),
                "kind": {"type": "dm"},
            },
        )
    after = _await_settled(url, initiated=before["initiated"] + 1)
    after["dialled_delta"] = after["dialled"] - before["dialled"]
    after["delivered_delta"] = after["delivered"] - before["delivered"]
    return after


# ── the device ────────────────────────────────────────────────────────────────


class _Device:
    """What a browser's ``PushManager`` holds for one subscription: a P-256
    keypair and a 16-byte auth secret (RFC 8291 § 2)."""

    def __init__(self):
        self.private_key = ec.generate_private_key(ec.SECP256R1())
        self.public_bytes = self.private_key.public_key().public_bytes(
            encoding=Encoding.X962, format=PublicFormat.UncompressedPoint
        )
        self.auth_secret = os.urandom(16)
        self.p256dh_b64 = _b64url(self.public_bytes)
        self.auth_b64 = _b64url(self.auth_secret)

    def decrypt(self, body: bytes) -> bytes:
        """RFC 8188 ``aes128gcm`` with the RFC 8291 key derivation — the
        receiving user agent's side."""
        salt, record_size, id_len = body[:16], int.from_bytes(body[16:20], "big"), body[20]
        sender_public = body[21 : 21 + id_len]
        ciphertext = body[21 + id_len :]
        assert len(ciphertext) <= record_size, "a single record is expected"

        shared = self.private_key.exchange(
            ec.ECDH(),
            ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), sender_public),
        )
        ikm = HKDF(
            algorithm=hashes.SHA256(),
            length=32,
            salt=self.auth_secret,
            info=b"WebPush: info\x00" + self.public_bytes + sender_public,
        ).derive(shared)
        cek = HKDF(
            algorithm=hashes.SHA256(), length=16, salt=salt,
            info=b"Content-Encoding: aes128gcm\x00",
        ).derive(ikm)
        nonce = HKDF(
            algorithm=hashes.SHA256(), length=12, salt=salt,
            info=b"Content-Encoding: nonce\x00",
        ).derive(ikm)
        padded = AESGCM(cek).decrypt(nonce, ciphertext, None)
        # The last record ends in a 0x02 delimiter followed by zero padding.
        stripped = padded.rstrip(b"\x00")
        assert stripped.endswith(b"\x02"), f"missing last-record delimiter: {padded!r}"
        return stripped[:-1]


def _assert_one_readable_push(
    url: str, decided_before: int, endpoint, device: _Device, vapid_public_key: str, *, trigger: str
) -> int:
    """``trigger`` made the nest decide exactly one dispatch, its POST arrived,
    and the device can read it. Returns the new decision count.

    Called after the sender's call returned, so the decision count is final for
    this trigger (docstring fact 1); only the POST itself is awaited. A
    duplicate POST that lands later than this read surfaces in the next
    trigger's read of the listener.
    """
    decided = _dispatches_decided(url)
    assert decided == decided_before + 1, (
        f"{trigger} for an actor with no live connection must make the nest "
        "decide exactly one push dispatch before it replies to the sender "
        f"(common.md § Dispatch Logic); it decided {decided - decided_before}"
    )
    wait_until(
        endpoint.has_new,
        PUSH_DIAL_S,
        diagnose=lambda: (
            f"the nest decided to push for {trigger} but no POST reached the "
            f"subscribed endpoint {endpoint.url}"
        ),
    )
    new = endpoint.take_new()
    assert len(new) == 1, (
        f"{trigger} must push exactly once to the subscribed endpoint; got {len(new)} POSTs"
    )
    headers, body = new[0]
    assert headers.get("content-encoding") == "aes128gcm", headers
    assert headers.get("ttl"), f"a web push carries a TTL: {headers}"

    # The VAPID signature a relay checks before accepting the push (RFC 8292).
    scheme, _, params = headers.get("authorization", "").partition(" ")
    assert scheme == "vapid", f"expected a VAPID Authorization header: {headers}"
    fields = dict(p.split("=", 1) for p in params.split(","))
    assert fields.get("k") == vapid_public_key, (
        "the push must be signed with the VAPID key the nest publishes through "
        f"fauna.push.vapid_key: header k={fields.get('k')!r}, published {vapid_public_key!r}"
    )
    _verify_vapid_jwt(fields["t"], vapid_public_key)

    plaintext = json.loads(device.decrypt(body))
    assert plaintext.get("title") and plaintext.get("body"), (
        f"the decrypted push must carry a title and body to show: {plaintext!r}"
    )
    return decided


def _verify_vapid_jwt(token: str, vapid_public_key: str) -> None:
    header_b64, claims_b64, sig_b64 = token.split(".")
    raw = _b64url_decode(sig_b64)
    signature = encode_dss_signature(
        int.from_bytes(raw[:32], "big"), int.from_bytes(raw[32:], "big")
    )
    public_key = ec.EllipticCurvePublicKey.from_encoded_point(
        ec.SECP256R1(), _b64url_decode(vapid_public_key)
    )
    public_key.verify(
        signature, f"{header_b64}.{claims_b64}".encode(), ec.ECDSA(hashes.SHA256())
    )
    claims = json.loads(_b64url_decode(claims_b64))
    assert claims.get("aud") == "http://127.0.0.1", (
        f"the JWT audience must be the endpoint's origin: {claims!r}"
    )


# ── the push service's side ───────────────────────────────────────────────────


class _PushEndpoint:
    """A loopback web-push endpoint: records each POST and answers ``201
    Created`` the way a push service accepts a message (RFC 8030 § 5)."""

    def __init__(self):
        self.received: list[tuple[dict, bytes]] = []
        self._taken = 0
        received, lock = self.received, threading.Lock()

        class _Handler(BaseHTTPRequestHandler):
            def do_POST(self):  # noqa: N802 — http.server's naming
                body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                with lock:
                    received.append(({k.lower(): v for k, v in self.headers.items()}, body))
                self.send_response(201)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def log_message(self, *_args):
                pass

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
        self.url = f"http://127.0.0.1:{self._server.server_address[1]}/push/{uuid.uuid4().hex}"
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    def has_new(self) -> bool:
        return len(self.received) > self._taken

    def take_new(self) -> list[tuple[dict, bytes]]:
        new = self.received[self._taken :]
        self._taken = len(self.received)
        return new

    def __enter__(self):
        self._thread.start()
        return self

    def __exit__(self, *_exc):
        self._server.shutdown()
        self._server.server_close()


# ── wire helpers ──────────────────────────────────────────────────────────────


def _send_email(url: str, sender: dict, recipient: dict, what: str) -> dict:
    payload, _post_id = build_email_inbox_payload(
        sender["signing_key"],
        recipient["actor_id_hex"],
        f"push dispatch: {what}",
        f"Sent while the recipient's app is {what}.",
        node_url=url,
    )
    with _actor_client(url, sender) as sc:
        return sc.call(
            "fauna.inbox.send",
            {
                "recipient_actor_id": recipient["actor_id_hex"],
                "recipient_nest_url": None,
                "payload_bytes": payload,
            },
        )


def _counters(url: str) -> dict:
    """The nest's push dispatch counters (``push.rs``
    ``PushService::dispatch_counters``): ``initiated`` bumped inside the
    sender's handler before its reply; ``dialled`` / ``delivered`` summed by
    the background dispatch, which bumps ``settled`` only afterwards."""
    with urllib.request.urlopen(f"{url}/api/v1/test/push/dispatches", timeout=RPC_ROUNDTRIP_S) as r:
        assert r.status == 200, (
            f"the dispatch counter hook returned {r.status} — is the nest built "
            "with `--features test-hooks`?"
        )
        counters = json.loads(r.read())
    assert counters["initiated"] is not None, "this nest runs no push service"
    return {k: int(v) for k, v in counters.items()}


def _dispatches_decided(url: str) -> int:
    """Push dispatches the nest has **decided** so far — bumped inside the
    sender's handler, before its reply."""
    return _counters(url)["initiated"]


def _await_settled(url: str, initiated: int | None = None) -> dict:
    """Wait until every decided dispatch has settled — its outcome summed into
    the counters — and return them. With ``initiated``, first assert that
    exactly that many were decided (the sender's reply already ordered it)."""
    if initiated is not None:
        decided = _dispatches_decided(url)
        assert decided == initiated, (
            f"exactly {initiated} dispatch decision(s) expected before the "
            f"sender's reply; the nest had made {decided}"
        )
    wait_until(
        lambda: (c := _counters(url))["settled"] == c["initiated"],
        PUSH_DIAL_S,
        diagnose=lambda: f"a decided push dispatch never settled: {_counters(url)!r}",
    )
    return _counters(url)


def _ws_connections(admin: WsRpcAdminClient) -> int:
    return int(admin.call("fauna.admin.stats", {})["ws_connections"])


def _await_connections(admin: WsRpcAdminClient, count: int) -> None:
    """The nest-wide connection count is back to ``count``: a closed client's
    connection has left the registry (the dedicated nest's connections are all
    this test's own)."""
    wait_until(
        lambda: _ws_connections(admin) == count,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the nest still counts {_ws_connections(admin)} live connection(s), "
            f"expected {count}: a closed client's connection never left the registry"
        ),
    )


def _await_offline(admin: WsRpcAdminClient, baseline: int) -> None:
    wait_until(
        lambda: _ws_connections(admin) == baseline,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the nest still counts {_ws_connections(admin)} live connection(s) "
            f"against the admin-only baseline of {baseline}: a closed client's "
            "connection never left the registry"
        ),
    )


def _actor_client(url: str, actor: dict) -> WsRpcAdminClient:
    return _client(url, actor["actor_id_bytes"], actor["signing_key"])


def _client(url: str, actor_id: bytes, signing_key: SigningKey) -> WsRpcAdminClient:
    return WsRpcAdminClient(url, actor_id=actor_id, signing_key=bytes(signing_key))


def _as_signing_key(raw) -> SigningKey:
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def _b64url_decode(text: str) -> bytes:
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))
