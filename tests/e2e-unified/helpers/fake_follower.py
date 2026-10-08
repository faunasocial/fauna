"""An in-test fediverse server that plays a remote follower of a nest user.

Shared by the two surfaces that need a fediverse peer which really receives:
the nest-side federation suite (`tests/api/test_activitypub_federation.py`) and
the app-surface witnesses that drive the same flows through an app's UI
(`tests/test_feed_fediverse_reply.py`). Lifted out of the api suite so one file
owns the peer — two copies of a signature verifier drift, and a drifted
verifier that passes proves nothing.

Everything here is headless: the follower serves an actor document (inbox URL +
RSA public key) the nest fetches and caches, records every activity delivered
to its inbox with its headers, and signs what it sends exactly as a real
draft-cavage fediverse server does.
"""

from __future__ import annotations

import base64
import hashlib
import http.server
import json
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from email.utils import formatdate

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa

# Every producer enqueues durably into `ap_delivery_queue` then nudges the
# worker (`bins/fauna-nest/src/activitypub/sync_worker.rs`), so activities
# arrive in well under a second; the worker's 30s poll is only the backstop.
# The generous budget is NOT load-bearing — if a delivery starts taking ~30s,
# a nudge was lost (that is the regression signal, not a slow test).
DELIVERY_TIMEOUT_S = 50


def _http_date() -> str:
    """An RFC 7231 HTTP-date — the format the nest's `format_http_date` emits."""
    return formatdate(timeval=None, localtime=False, usegmt=True)


def _digest_of(body: bytes) -> str:
    """`SHA-256=<base64>` — the shared crate's `compute_digest` format."""
    return "SHA-256=" + base64.b64encode(hashlib.sha256(body).digest()).decode()


def _signing_string(pairs: list[tuple[str, str]]) -> str:
    """draft-cavage signing string: `lowercase(name): value`, newline-joined.

    Mirrors `fauna_bridge_activitypub::http_signatures::build_signature_string`.
    """
    return "\n".join(f"{name.lower()}: {value}" for name, value in pairs)


def parse_signature_header(header: str) -> dict:
    """Parse a draft-cavage `Signature` header into its `keyId`/`headers`/… parts.

    The twin of the shared crate's `parse_signature_header`; splits on commas
    outside quoted strings.
    """
    parts, current, in_quotes = [], "", False
    for ch in header:
        if ch == '"':
            in_quotes = not in_quotes
            current += ch
        elif ch == "," and not in_quotes:
            if current.strip():
                parts.append(current.strip())
            current = ""
        else:
            current += ch
    if current.strip():
        parts.append(current.strip())

    out = {}
    for part in parts:
        if "=" in part:
            k, v = part.split("=", 1)
            out[k.strip()] = v.strip().strip('"')
    return out


def assert_valid_http_signature(entry: dict, pubkey_pem: str, path: str) -> None:
    """Assert the nest signed this inbox POST with the key its actor publishes.

    Reconstructs the draft-cavage signing string from the headers the
    `Signature` header names (the verifier half of
    `fauna_bridge_activitypub::http_signatures::verify_signature`) and checks
    the RSA PKCS#1 v1.5 / SHA-256 signature against `pubkey_pem` — the PEM
    served at the nest's own `/ap/users/{username}` actor document. A remote
    server that rejects our signature drops the activity, so this is the
    assertion that says "a real Mastodon would accept this POST".
    """
    headers = entry["headers"]
    sig_header = headers.get("signature")
    assert sig_header, f"delivery carried no Signature header: {sorted(headers)}"

    parsed = parse_signature_header(sig_header)
    assert parsed.get("algorithm") == "rsa-sha256", parsed
    signed_names = parsed["headers"].split()
    # (request-target) + host + date + digest is what the nest signs; anything
    # less would let a proxy rewrite the body or replay to another inbox.
    assert "(request-target)" in signed_names, signed_names
    assert "digest" in signed_names, signed_names

    pairs = []
    for name in signed_names:
        if name == "(request-target)":
            pairs.append((name, f"post {path}"))
        else:
            value = headers.get(name.lower())
            assert value is not None, f"signed header {name!r} absent from the request"
            pairs.append((name, value))

    # The Digest header must actually cover the delivered body.
    assert headers.get("digest") == _digest_of(entry["body"]), "digest does not cover the body"

    public_key = serialization.load_pem_public_key(pubkey_pem.encode())
    public_key.verify(
        base64.b64decode(parsed["signature"]),
        _signing_string(pairs).encode(),
        padding.PKCS1v15(),
        hashes.SHA256(),
    )  # raises InvalidSignature on mismatch


class FakeFollower:
    """An in-test fediverse server that plays a remote follower of a nest user.

    Serves exactly what the nest needs to treat it as a real remote actor:
    an actor document (inbox URL + RSA public key) at `actor_uri` — fetched and
    cached by `inbox_routes::fetch_remote_actor` to verify our signatures — and
    an inbox that records every delivered activity with its headers.
    """

    def __init__(
        self,
        port: int,
        username: str = "bob",
        *,
        require_signed_get: bool = False,
        require_user_agent: bool = False,
    ):
        self.port = port
        self.username = username
        self.actor_uri = f"http://127.0.0.1:{port}/users/{username}"
        self.inbox_url = f"{self.actor_uri}/inbox"
        # The profile fields a real actor document carries (`name`, `icon`) —
        # what the nest projects as the bridged-author face (bridges.md
        # § Unified feed ingestion → *Bridged authors*). Nothing fetches the
        # icon; it only has to be an https URL the shared media proxy accepts.
        self.display_name = f"{username.capitalize()} of the Fediverse"
        self.icon_url = f"https://follower.invalid/avatars/{username}.png"
        self.received: list[dict] = []
        # Secure mode (`AUTHORIZED_FETCH`): refuse to serve our own actor
        # document to an unsigned GET, as a strict fediverse peer does. The
        # headless stand-in for the real-Mastodon strict harness.
        self.require_signed_get = require_signed_get
        # Refuse a request that carries no `User-Agent`, which is what
        # GoToSocial does — with `418 I'm a teapot`. The headless stand-in for
        # the GoToSocial interop harness, and not a hypothetical: our outbound
        # AP client sent no UA at all until the real peer refused it, and
        # Mastodon had tolerated the omission for the whole life of the feature.
        self.require_user_agent = require_user_agent
        # Headers of every actor GET we served or refused, so a test can assert
        # what the nest actually sent rather than infer it from the outcome.
        self.actor_gets: list[dict] = []
        self._lock = threading.Lock()
        self._key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        self.public_pem = (
            self._key.public_key()
            .public_bytes(
                encoding=serialization.Encoding.PEM,
                format=serialization.PublicFormat.SubjectPublicKeyInfo,
            )
            .decode()
        )
        self._server = http.server.ThreadingHTTPServer(
            ("127.0.0.1", port), self._handler_class()
        )
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    # ── lifecycle ──

    def __enter__(self):
        self._thread.start()
        return self

    def __exit__(self, *_exc):
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)

    # ── the actor document `fetch_remote_actor` reads ──

    def actor_document(self) -> dict:
        return {
            "@context": [
                "https://www.w3.org/ns/activitystreams",
                "https://w3id.org/security/v1",
            ],
            "type": "Person",
            "id": self.actor_uri,
            "preferredUsername": self.username,
            "name": self.display_name,
            "icon": {"type": "Image", "mediaType": "image/png", "url": self.icon_url},
            "inbox": self.inbox_url,
            "outbox": f"{self.actor_uri}/outbox",
            "publicKey": {
                "id": f"{self.actor_uri}#main-key",
                "owner": self.actor_uri,
                "publicKeyPem": self.public_pem,
            },
        }

    def _handler_class(self):
        follower = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass  # keep pytest output readable

            def do_GET(self):
                if self.path != f"/users/{follower.username}":
                    self.send_error(404)
                    return
                follower._record_actor_get(self.headers)
                if follower.require_user_agent and not self.headers.get("User-Agent"):
                    # GoToSocial's actual refusal, status and body both. Like the
                    # 401 below it is JSON, so a fetch that ignores the status
                    # would parse this into an "actor" with empty fields.
                    body = json.dumps(
                        {"error": "I'm a teapot: no user-agent sent with request"}
                    ).encode()
                    self.send_response(418)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                if follower.require_signed_get and "Signature" not in self.headers:
                    # A real secure-mode peer answers with a JSON error body,
                    # which is exactly what made this failure invisible: JSON
                    # parses, so a fetch that ignores the status turns the
                    # refusal into an "actor" whose fields are all empty.
                    body = json.dumps({"error": "Request not signed"}).encode()
                    self.send_response(401)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                doc = json.dumps(follower.actor_document()).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/activity+json")
                self.send_header("Content-Length", str(len(doc)))
                self.end_headers()
                self.wfile.write(doc)

            def do_POST(self):
                if self.path != f"/users/{follower.username}/inbox":
                    self.send_error(404)
                    return
                length = int(self.headers.get("Content-Length", 0))
                body = self.rfile.read(length)
                follower._record(self.headers, body)
                self.send_response(202)
                self.send_header("Content-Length", "0")
                self.end_headers()

        return Handler

    def _record_actor_get(self, headers) -> None:
        with self._lock:
            self.actor_gets.append({k.lower(): v for k, v in headers.items()})

    def _record(self, headers, body: bytes) -> None:
        try:
            activity = json.loads(body)
        except json.JSONDecodeError:
            activity = {}
        # HTTP header names are case-insensitive and the nest's client sends
        # them lower-cased; key on lower-case so lookups don't depend on that.
        with self._lock:
            self.received.append(
                {
                    "headers": {k.lower(): v for k, v in headers.items()},
                    "body": body,
                    "activity": activity,
                }
            )

    # ── driving the nest ──

    def post_signed(self, url: str, activity: dict) -> int:
        """Sign `activity` like a real fediverse server and POST it to `url`.

        Produces the draft-cavage signature the nest's inbox pipeline verifies
        against the public key it fetches from `actor_document()`.
        """
        parsed = urllib.parse.urlparse(url)
        host, path = parsed.netloc, parsed.path
        body = json.dumps(activity).encode()
        digest, date = _digest_of(body), _http_date()

        sig = self._key.sign(
            _signing_string(
                [
                    ("(request-target)", f"post {path}"),
                    ("host", host),
                    ("date", date),
                    ("digest", digest),
                ]
            ).encode(),
            padding.PKCS1v15(),
            hashes.SHA256(),
        )
        signature_header = (
            f'keyId="{self.actor_uri}#main-key",algorithm="rsa-sha256",'
            f'headers="(request-target) host date digest",'
            f'signature="{base64.b64encode(sig).decode()}"'
        )
        req = urllib.request.Request(
            url,
            data=body,
            method="POST",
            headers={
                "Content-Type": "application/activity+json",
                "Host": host,
                "Date": date,
                "Digest": digest,
                "Signature": signature_header,
            },
        )
        try:
            with urllib.request.urlopen(req) as resp:
                return resp.status
        except urllib.error.HTTPError as e:
            raise AssertionError(
                f"nest rejected our signed {activity.get('type')}: "
                f"HTTP {e.code} {e.read().decode(errors='replace')}"
            ) from e

    # ── observing deliveries ──

    def wait_for(
        self,
        activity_type: str,
        timeout: float = DELIVERY_TIMEOUT_S,
        where=None,
    ) -> dict:
        """Block until an activity of `activity_type` is delivered; return it.

        `where`, when given, is a predicate over the activity dict: the first
        delivered activity of the type it accepts is returned — how a caller
        tells two `Create`s on one inbox apart (a reply, then a quote).
        """
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self._lock:
                for entry in self.received:
                    activity = entry["activity"]
                    if activity.get("type") == activity_type and (
                        where is None or where(activity)
                    ):
                        return entry
            time.sleep(0.25)
        with self._lock:
            seen = [e["activity"].get("type") for e in self.received]
        raise AssertionError(
            f"no {activity_type}{' matching the predicate' if where else ''} "
            f"delivered to {self.inbox_url} within {timeout}s; "
            f"activities received: {seen or 'none'}"
        )
