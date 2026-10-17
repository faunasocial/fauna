"""The nest's push dispatch, triggered and read the way a witness needs it.

The causal anchors (``docs/goal/architecture/apps/common.md`` § Dispatch Logic;
why they hold: ``tests/api/test_push_dispatch.py``'s docstring, facts 1–2):

* **the decision** is synchronous — ``initiated`` is bumped inside the
  sender's handler, before its reply;
* **the dispatch** is not — its background task sums what it dialled or
  delivered and bumps ``settled`` only afterwards, so once ``settled`` reaches
  ``initiated`` the sums are final;
* **presence** — which of an actor's devices the dispatch would find present —
  is the nest's own snapshot, the dispatch's input. A client announces it
  asynchronously on every (re)connect, so a witness expecting a ``ws-device``
  row to be delivered waits for it before triggering.

All three are ``test-hooks`` HTTP reads (``bins/fauna-nest/src/push_test_hooks.rs``),
so a caller runs only against a nest this harness built with that feature.

The trigger is a production one: a mail sent over ``fauna.inbox.send`` by
another actor, which the nest holds as a knock (a stranger under the default
``allow_knock`` mode) or delivers (an ``open`` inbox) — and pushes either way.
"""

from __future__ import annotations

import base64
import json
import os
import threading
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.asymmetric.utils import encode_dss_signature
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import build_email_inbox_payload
from helpers.budgets import PUSH_DIAL_S, RPC_ROUNDTRIP_S
from helpers.waiting import wait_until


def _get(url: str, path: str) -> dict:
    with urllib.request.urlopen(f"{url}{path}", timeout=RPC_ROUNDTRIP_S) as r:
        assert r.status == 200, (
            f"{path} returned {r.status} — is the nest built with `--features test-hooks`?"
        )
        return json.loads(r.read())


def counters(url: str) -> dict:
    """The nest's push dispatch counters (``push.rs``
    ``PushService::dispatch_counters``): ``initiated`` bumped inside the
    sender's handler before its reply; ``dialled`` / ``delivered`` summed by
    the background dispatch, which bumps ``settled`` only afterwards."""
    raw = _get(url, "/api/v1/test/push/dispatches")
    assert raw["initiated"] is not None, "this nest runs no push service"
    return {k: int(v) for k, v in raw.items()}


def decided(url: str) -> int:
    """Push dispatches the nest has **decided** so far — bumped inside the
    sender's handler, before its reply."""
    return counters(url)["initiated"]


def await_settled(url: str, initiated: int | None = None) -> dict:
    """Wait until every decided dispatch has settled — its outcome summed into
    the counters — and return them. With ``initiated``, first assert that
    exactly that many were decided (the sender's reply already ordered it)."""
    if initiated is not None:
        made = decided(url)
        assert made == initiated, (
            f"exactly {initiated} dispatch decision(s) expected before the "
            f"sender's reply; the nest had made {made}"
        )
    wait_until(
        lambda: (c := counters(url))["settled"] == c["initiated"],
        PUSH_DIAL_S,
        diagnose=lambda: f"a decided push dispatch never settled: {counters(url)!r}",
    )
    return counters(url)


def presence(url: str, actor_id_hex: str) -> dict:
    """Which of the actor's devices are present right now:
    ``{"announced": [device_id…], "app": [device_id…], "stand_in": [device_id…],
    "any_unannounced": bool}`` — ``announced`` is every announced device of
    either kind, ``app`` and ``stand_in`` the two kinds apart."""
    return _get(url, f"/api/v1/test/push/presence?actor_id={actor_id_hex}")


def await_present(url: str, actor_id_hex: str, device_id: str, budget: float) -> None:
    """Wait until a live connection of the actor has announced ``device_id``."""
    wait_until(
        lambda: device_id in presence(url, actor_id_hex)["announced"],
        budget,
        diagnose=lambda: (
            f"no live connection of {actor_id_hex[:12]}… announced device "
            f"{device_id!r}; the nest's presence reads {presence(url, actor_id_hex)!r}"
        ),
    )


def actor_client(url: str, actor: dict) -> WsRpcAdminClient:
    """A WS-RPC client signing as ``actor`` (a ``create_actor_and_register`` dict)."""
    return WsRpcAdminClient(
        url, actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"])
    )


def send_inbox_mail(url: str, sender: dict, recipient: dict, what: str) -> dict:
    """``sender`` mails ``recipient`` over ``fauna.inbox.send``; returns the
    nest's reply (``inbox_id`` is ``None`` when the mail was held as a knock)."""
    payload, _post_id = build_email_inbox_payload(
        sender["signing_key"],
        recipient["actor_id_hex"],
        f"push dispatch: {what}",
        f"Sent while the recipient's app is {what}.",
        node_url=url,
    )
    with actor_client(url, sender) as sc:
        return sc.call(
            "fauna.inbox.send",
            {
                "recipient_actor_id": recipient["actor_id_hex"],
                "recipient_nest_url": None,
                "payload_bytes": payload,
            },
        )


def await_absent(url: str, actor_id_hex: str, budget: float) -> None:
    """Wait until the actor holds no live connection the dispatch would count —
    none announced, none unannounced — so every relay row of theirs is dialled.
    The read the actor's own app cannot make (any connection of theirs is
    itself present), unlike :func:`await_offline`'s nest-wide count."""
    wait_until(
        lambda: (p := presence(url, actor_id_hex))["announced"] == []
        and not p["any_unannounced"],
        budget,
        diagnose=lambda: (
            f"{actor_id_hex[:12]}… still holds a live connection; the nest's "
            f"presence reads {presence(url, actor_id_hex)!r}"
        ),
    )


# ── the device ────────────────────────────────────────────────────────────────


class WebPushDevice:
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


def assert_one_readable_push(
    endpoint: "PushEndpoint", device: WebPushDevice, vapid_public_key: str, *, trigger: str
) -> dict:
    """``trigger`` made exactly one POST reach ``endpoint``, signed with the
    VAPID key the nest publishes, and ``device`` can read it. Returns the
    decrypted payload.

    The caller has already established that a dispatch was decided (its own
    causal barrier); only the POST itself is awaited here. A duplicate POST
    that lands later than this read surfaces in the next read of the listener.
    """
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
    return plaintext


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


class PushEndpoint:
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


# ── connection counts ─────────────────────────────────────────────────────────


def ws_connections(admin: WsRpcAdminClient) -> int:
    """The nest-wide live connection count (``fauna.admin.stats``)."""
    return int(admin.call("fauna.admin.stats", {})["ws_connections"])


def await_offline(admin: WsRpcAdminClient, baseline: int) -> None:
    """The nest-wide connection count is back to the admin-only ``baseline``:
    a closed client's connection has left the registry (exact only on a
    dedicated nest whose every connection is the test's own)."""
    wait_until(
        lambda: ws_connections(admin) == baseline,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the nest still counts {ws_connections(admin)} live connection(s) "
            f"against the admin-only baseline of {baseline}: a closed client's "
            "connection never left the registry"
        ),
    )


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def _b64url_decode(text: str) -> bytes:
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))
