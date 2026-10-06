"""tier_3 API e2e: the **third-party events doors** — "something changed" for
a connected app, never what (``docs/goal/architecture/transport.md`` § Push
events → *Third-party event doors*; the scope arm,
``docs/goal/behavior/authorization-server.md`` § Scope grammar).

Two admitted publishers on one nest, each a real ``https`` document with a
manifest signed in Python (``helpers.client_metadata_server``, one CA for
both hosts):

- app A (``example.com``) consents ``fauna:records:rw:ext.example.com.*`` and
  ``fauna:events:subscribe``; app B (``example.org``) consents only its own
  records;
- **the filter, by a causal barrier:** B writes its kind first, then A writes
  its own. A's principal session receives A's nudge — the scope and nothing
  else — and since a leaked frame for B's earlier write would have been
  queued on A's socket ahead of it, receiving A's frame first proves B's was
  filtered;
- **the HTTP door:** ``GET /api/v1/events`` (DPoP-bound) names A's scope and
  not B's, and a long-poll from that cursor is answered by A's next write;
- **the webhook:** both documents declare an ``events_uri`` on their own
  host, served by this test's metadata server. A's server receives a
  ``POST`` carrying a security event token (RFC 8417, ``typ: secevent+jwt``,
  RFC 8935 push) that verifies against the key set at ``/oauth/jwks`` — as a
  remote server would obtain it — naming A's ``client_id``, the ``sub`` A's
  own token carries, and a cursor past B's write: a notification minted for
  B's earlier write would carry A's reach from before A wrote (the causal
  barrier again). B, who never subscribed, is POSTed nothing.
"""

import base64
import json
import secrets
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

import pytest
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec, ed25519, x25519
from cryptography.hazmat.primitives.asymmetric.utils import encode_dss_signature

import fauna_ffi
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.client_metadata_server import (
    ClientMetadataServer,
    client_document,
    manifest_payload,
    sign_manifest,
)
from helpers.oauth_client import NONCE_HEADER, OAuthClient
from tests.api.test_ext_kinds import _approve_with_grant, _put, _raw, _secret
from tests.api.test_third_party_session import _Session, _upgrade

pytestmark = pytest.mark.tier_3

A_HOST, B_HOST = "example.com", "example.org"
A_NOTES, B_NOTES = f"ext.{A_HOST}.notes", f"ext.{B_HOST}.notes"
A_RECORDS, B_RECORDS = f"fauna:records:rw:ext.{A_HOST}.*", f"fauna:records:rw:ext.{B_HOST}.*"
EVENTS = "fauna:events:subscribe"
A_SCOPE = f"{A_RECORDS} {EVENTS}"
A_PATH, B_PATH = "/a-client.json", "/b-client.json"
#: Where each publisher's server takes the webhook — its own host, the one
#: place a manifest may aim it (``validate_events_uri``).
A_HOOK_PATH, B_HOOK_PATH = "/a-fauna-events", "/b-fauna-events"
REDIRECT_URI = "http://127.0.0.1:17775/callback"
EVENTS_PATH = "/api/v1/events"
#: The one event type the nest emits (``events_webhook::SCOPE_CHANGED_EVENT``).
SCOPE_CHANGED_EVENT = "urn:fauna:event-type:scope-changed"
#: How long the long-poll is asked to hold — the door's own ceiling. The
#: assertion is on what it answers, never on how long it took.
LONG_POLL_WAIT_S = 25
#: How long the webhook is given to arrive: the delivery is spawned off the
#: put path and dials through the guard, so one roundtrip budget.
WEBHOOK_WAIT_S = RPC_ROUNDTRIP_S


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(
        A_HOST, tmp_path_factory.mktemp("client-metadata"), extra_hosts=(B_HOST,)
    )
    for host, path, scope, hook in (
        (A_HOST, A_PATH, A_SCOPE, A_HOOK_PATH),
        (B_HOST, B_PATH, B_RECORDS, B_HOOK_PATH),
    ):
        key = ed25519.Ed25519PrivateKey.generate()
        payload = manifest_payload(host, key, ["notes"]) | {"events_uri": f"https://{host}{hook}"}
        manifest = sign_manifest(key, payload)
        server.documents[path] = client_document(
            server.client_id(path, host), REDIRECT_URI, scope, manifest_jws=manifest
        )
    yield server
    server.close()


@pytest.fixture(scope="module")
def consent_nest_env(metadata_server):
    return metadata_server.nest_env()


class _App:
    """One consented connected app: its client, keys, grant and token."""

    def __init__(self, nest, metadata_server, host: str, path: str, scope: str, kind: str):
        self.holder = x25519.X25519PrivateKey.generate()
        self.writer = ed25519.Ed25519PrivateKey.generate()
        self.writer_pub = _raw(self.writer.public_key())
        self.kind = kind
        self.scope = f"ext:{kind}"
        self.client = OAuthClient(
            base=nest["url"],
            htu_origin=f"https://{HANDLE_DOMAIN}",
            redirect_uri=REDIRECT_URI,
            scope=scope,
            holder_x25519=_raw(self.holder.public_key()),
            writer_ed25519=self.writer_pub,
            client_id_url=metadata_server.client_id(path, host),
        )
        tokens = _approve_with_grant(
            nest, self.client, _raw(self.holder.public_key()), self.writer_pub,
            secrets.token_bytes(16), kinds=(kind,),
        )
        self.token = tokens["access_token"]
        self.session = _Session(_upgrade(nest, self.client, self.token))
        ok, reply = self.session.call("fauna.capabilities.fetch", {})
        assert ok and len(reply.get("grants", [])) == 1, reply
        self.grant = bytes(reply["grants"][0])
        self.seq = 0

    def write(self):
        """Seal and put one row of the app's own kind, as its own writer."""
        self.seq += 1
        item_key, envelope = fauna_ffi.ext_kind_seal_row(
            self.grant, _secret(self.holder), _secret(self.writer), self.kind,
            f"row-{self.seq}", b"hello", self.seq, int(time.time() * 1000),
        )
        ok, reply = _put(self.session, self.scope, self.writer_pub, item_key, envelope, self.seq)
        assert ok, f"{self.kind}: its own kind, as its own writer: {reply!r}"


def _events(nest, app: _App, cursor: int | None, wait: int) -> tuple[int, dict]:
    """``GET /api/v1/events`` with the app's token and a fresh DPoP proof; one
    ``use_dpop_nonce`` retry, the protocol's own."""
    query = {"wait": wait} | ({} if cursor is None else {"cursor": cursor})
    url = f"{nest['url']}{EVENTS_PATH}?{urllib.parse.urlencode(query)}"
    htu = f"https://{HANDLE_DOMAIN}{EVENTS_PATH}"

    def send():
        request = urllib.request.Request(
            url,
            method="GET",
            headers={
                "Authorization": f"DPoP {app.token}",
                "DPoP": app.client.dpop_proof("GET", htu, access_token=app.token),
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=wait + RPC_ROUNDTRIP_S) as resp:
                return resp.status, dict(resp.headers), json.loads(resp.read() or b"{}")
        except urllib.error.HTTPError as refused:
            raw = refused.read()
            return refused.code, dict(refused.headers), json.loads(raw) if raw else {}

    status, headers, body = send()
    lowered = {k.lower(): v for k, v in headers.items()}
    if status == 401 and "use_dpop_nonce" in lowered.get("www-authenticate", ""):
        app.client.nonce = lowered.get(NONCE_HEADER.lower())
        status, _, body = send()
    return status, body


def _b64d(segment: str) -> bytes:
    return base64.urlsafe_b64decode(segment + "=" * (-len(segment) % 4))


def _jwt_claims(token: str) -> dict:
    """A JWT's claim set, unverified — for reading what a token the nest
    minted says."""
    return json.loads(_b64d(token.split(".")[1]))


def _verify_event_token(token: str, jwks: dict) -> dict:
    """Verify a security event token as a remote server would: the pinned
    ``alg`` and ``typ``, the ``kid``'s key from the issuer's served set, ES256
    over the signing input. Returns the claim set."""
    header_b64, claims_b64, sig_b64 = token.split(".")
    header = json.loads(_b64d(header_b64))
    assert header["alg"] == "ES256" and header["typ"] == "secevent+jwt", header
    key = next(k for k in jwks["keys"] if k["kid"] == header["kid"])
    public = ec.EllipticCurvePublicNumbers(
        int.from_bytes(_b64d(key["x"]), "big"),
        int.from_bytes(_b64d(key["y"]), "big"),
        ec.SECP256R1(),
    ).public_key()
    raw = _b64d(sig_b64)
    assert len(raw) == 64, "JOSE ES256 is the raw r‖s"
    public.verify(
        encode_dss_signature(int.from_bytes(raw[:32], "big"), int.from_bytes(raw[32:], "big")),
        f"{header_b64}.{claims_b64}".encode(),
        ec.ECDSA(hashes.SHA256()),
    )
    return json.loads(_b64d(claims_b64))


def _jwks(nest) -> dict:
    with urllib.request.urlopen(f"{nest['url']}/oauth/jwks", timeout=RPC_ROUNDTRIP_S) as resp:
        return json.loads(resp.read())


@pytest.mark.feature("connected-apps")
def test_a_connected_app_hears_only_its_own_scopes_change(
    consent_nest, consent_bridge, metadata_server
):
    nest = consent_nest
    a = _App(nest, metadata_server, A_HOST, A_PATH, A_SCOPE, A_NOTES)
    b = _App(nest, metadata_server, B_HOST, B_PATH, B_RECORDS, B_NOTES)

    # Nothing has moved yet: the cursor every later assertion is measured from.
    status, idle = _events(nest, a, None, wait=0)
    assert status == 200 and idle["frames"] == [], idle

    # ── WS-RPC push, with the causal barrier: B first, then A. ──
    b.write()
    # B's row moved the account's log past the idle cursor, and A's filter
    # emptied the page — the cursor the webhook's notification must exceed.
    status, after_b = _events(nest, a, None, wait=0)
    assert status == 200 and after_b["frames"] == [], after_b
    assert after_b["cursor"] > idle["cursor"], (idle, after_b)
    a.write()
    kind, payload = a.session.next_push()
    assert kind == "fauna.sync.changed", (kind, payload)
    # The scope and nothing else: `folder` is a required wire field, left
    # empty — no folder name, no hash address.
    assert payload.get("scope") == a.scope and not payload.get("folder"), (
        f"the first frame A hears is its own scope's nudge — a leaked frame for "
        f"B's earlier write would have come first: {payload!r}"
    )
    assert set(payload) <= {"scope", "folder"}, f"the scope and nothing else: {payload!r}"

    # ── The HTTP door: the same filter, and a cursor. ──
    status, first = _events(nest, a, None, wait=0)
    assert status == 200, first
    assert first["frames"] == [{"scope": a.scope}], (
        f"the poll names A's scope and never B's: {first!r}"
    )

    held: dict = {}

    def long_poll():
        held["reply"] = _events(nest, a, first["cursor"], wait=LONG_POLL_WAIT_S)

    poller = threading.Thread(target=long_poll)
    poller.start()
    # B's write alone must not answer A's long-poll with a frame; A's does.
    b.write()
    a.write()
    poller.join(timeout=LONG_POLL_WAIT_S + 2 * RPC_ROUNDTRIP_S)
    assert not poller.is_alive(), "the long-poll answered"
    status, after = held["reply"]
    assert status == 200, after
    assert after["frames"] == [{"scope": a.scope}], (
        f"the long-poll from the cursor names A's next write and nothing of B's: {after!r}"
    )
    assert after["cursor"] > first["cursor"], after

    # A session whose token never subscribed reaches no poll.
    status, refused = _events(nest, b, None, wait=0)
    assert status == 403 and refused.get("error", "").endswith("permission_denied"), (
        f"no events arm, no door: {status} {refused!r}"
    )

    # ── The webhook: A's server was told, by a token it can verify, and
    # told nothing more than to come and fetch. ──
    posts = metadata_server.wait_posts(A_HOOK_PATH, 1, timeout=WEBHOOK_WAIT_S)
    assert posts, "A's events_uri was POSTed after A's own writes"
    jwks = _jwks(nest)
    a_sub = _jwt_claims(a.token)["sub"]
    for _path, headers, body in posts:
        assert headers.get("content-type") == "application/secevent+jwt", headers
        claims = _verify_event_token(body.decode(), jwks)
        assert claims["iss"] == f"https://{HANDLE_DOMAIN}", claims
        assert claims["aud"] == a.client.client_id_url, claims
        assert claims["sub"] == a_sub, (claims, a_sub)
        assert set(claims) == {"iss", "aud", "sub", "iat", "jti", "events"}, (
            f"payload-free: nothing beyond who, whom and where to poll from: {claims!r}"
        )
        assert set(claims["events"]) == {SCOPE_CHANGED_EVENT}, claims
        event = claims["events"][SCOPE_CHANGED_EVENT]
        assert set(event) == {"cursor"}, f"no scope, no key, no content: {event!r}"
        assert event["cursor"] > after_b["cursor"], (
            f"a notification explained only by A's own write — one minted for B's "
            f"earlier write would carry A's reach from before A wrote: {event!r} vs {after_b!r}"
        )
    assert not [p for p in metadata_server.posts if p[0] == B_HOOK_PATH], (
        "B declared a webhook but never subscribed: nothing reaches it"
    )
