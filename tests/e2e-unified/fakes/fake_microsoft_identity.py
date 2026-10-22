"""A minimal TLS stand-in for the Microsoft identity platform's device-code
grant — the far end the Outlook import's **Sign in with Microsoft** talks to.

`docs/goal/behavior/mailbox-migration.md` § Microsoft sign-in — the device-code
grant: the app POSTs `client_id` and `scope` to `<authority>/devicecode`, shows
the `user_code` and `verification_uri` it gets back, and polls
`<authority>/token` with `grant_type=urn:ietf:params:oauth:grant-type:device_code`
every `interval` seconds until a token arrives. No suite may reach Microsoft, so
this fake is that authority, exactly as `fake_imap_source` is the foreign
mailbox: everything else in the run (app, shared-Rust flow, IMAP client, nest)
is real, which keeps a test using it tier_3.

**It speaks only what the client sends** — the two form POSTs of
`libs/fauna-mail/src/imap_client/oauth_device.rs` (the flow's `start` and `poll`
and the reconnect's `refresh`), and nothing else. Anything else is a `404` with
an error body, never a hang (convention 11 applied to a fake).

**The app is pointed at it** through the fifth gated seed,
`FAUNA_E2E_MICROSOFT_AUTHORITY` = :attr:`FakeMicrosoftIdentity.authority`
(`docs/goal/architecture/e2e-gated-seeds.md` § The Microsoft authority seed),
and trusts its certificate through `FAUNA_E2E_IMAP_EXTRA_CA_PEM` — so the fake
serves the session chain `fake_imap_source.session_cert` mints, and no second
trust variable exists. The seed redirects the authority alone: the client ID
and the scopes the app sends are recorded, never checked, so a witness can say
which registration asked.

**The test decides how a sign-in ends.** Every code answers
`authorization_pending` until the test calls :meth:`approve` (then a
distinctive bearer, :attr:`access_token`), :meth:`decline`
(`authorization_declined`), :meth:`expire` (`expired_token`) or
:meth:`refuse_consent` (Microsoft's `AADSTS65001` consent refusal) — so a
witness acts on the client's own progress (:meth:`wait_for_code`), never on time
(convention 14). `interval` is 1 second, the fastest RFC 8628 allows a client to
honour.

**Every request is recorded** — :attr:`device_code_requests` (the form fields,
`client_id` and `scope` among them) and :attr:`token_requests` (each poll's
`grant_type`; the device code and refresh token left out) — and every token it
issued is in :attr:`issued_tokens`, which is what outcome 18's black-box read
of the nest's files searches for.
"""

from __future__ import annotations

import json
import secrets
import ssl
import tempfile
import threading
import time
import urllib.parse
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from fake_imap_source import mint_localhost_cert

# The address the fake tells the app to visit — never dialled by anything; the
# test approves the code itself.
VERIFICATION_URI = "https://microsoft.example/devicelogin"

DEVICE_CODE_GRANT = "urn:ietf:params:oauth:grant-type:device_code"

# How a pending code ends, set by the test.
_PENDING = "pending"
_APPROVED = "approved"


@dataclass
class _Code:
    user_code: str
    device_code: str
    outcome: str = _PENDING


class FakeMicrosoftIdentity:
    """A loopback HTTPS server answering `/devicecode` and `/token` under any
    path prefix — the authority base the seed names, `/common/oauth2/v2.0`
    by default.

    Not a fake of a fauna binary — a stand-in for the third-party identity
    platform on the far side of the Outlook sign-in.
    """

    def __init__(
        self,
        *,
        cert: tuple[str, str] | None = None,
        host: str = "127.0.0.1",
        port: int = 0,
        interval: int = 1,
        expires_in: int = 900,
        token_lifetime: int = 3600,
    ) -> None:
        self.interval = interval
        self.expires_in = expires_in
        self.token_lifetime = token_lifetime
        # Distinctive, so a byte search of the nest's files cannot hit it by
        # accident (outcome 18).
        self.access_token = f"fauna-e2e-ms-access-{secrets.token_hex(16)}"
        self.refresh_token = f"fauna-e2e-ms-refresh-{secrets.token_hex(16)}"

        self.device_code_requests: list[dict[str, str]] = []
        self.token_requests: list[dict[str, str]] = []
        self.issued_tokens: list[str] = []
        self._codes: dict[str, _Code] = {}
        self._by_device: dict[str, _Code] = {}
        self._lock = threading.Lock()
        self._code_issued = threading.Condition(self._lock)

        # `cert` pins the session chain the app was launched trusting
        # (`fake_imap_source.session_cert`); left None, each instance mints
        # its own — right for a self-test, wrong for anything an app verifies.
        self.cert_pem, key_pem = cert if cert is not None else mint_localhost_cert()
        self._tmp = tempfile.TemporaryDirectory(prefix="fake-ms-identity-")
        cert_path = Path(self._tmp.name) / "cert.pem"
        key_path = Path(self._tmp.name) / "key.pem"
        cert_path.write_text(self.cert_pem)
        key_path.write_text(key_pem)
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(str(cert_path), str(key_path))

        fake = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:  # noqa: N802 — http.server's name
                length = int(self.headers.get("Content-Length") or 0)
                form = dict(
                    urllib.parse.parse_qsl(self.rfile.read(length).decode("utf-8"))
                )
                status, body = fake._answer(self.path, form)
                payload = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *_args: object) -> None:
                # A token travels in these requests: nothing goes to stderr.
                pass

        self._server = ThreadingHTTPServer((host, port), Handler)
        self._server.daemon_threads = True
        self._server.socket = ctx.wrap_socket(self._server.socket, server_side=True)
        self.host, self.port = self._server.server_address[:2]
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    @property
    def authority(self) -> str:
        """The base URL to seed as `FAUNA_E2E_MICROSOFT_AUTHORITY` — the
        flow appends `/devicecode` and `/token` to it."""
        return f"https://{self.host}:{self.port}/common/oauth2/v2.0"

    def start(self) -> "FakeMicrosoftIdentity":
        self._thread.start()
        return self

    def stop(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._tmp.cleanup()

    def __enter__(self) -> "FakeMicrosoftIdentity":
        return self.start()

    def __exit__(self, *_exc: object) -> None:
        self.stop()

    # ── the test's half ───────────────────────────────────────────────

    @property
    def issued_codes(self) -> list[str]:
        """Every `user_code` handed out, oldest first."""
        with self._lock:
            return list(self._codes)

    def wait_for_code(self, *, count: int = 1, timeout: float = 60.0) -> str:
        """Block until the app has asked for its `count`-th code and return
        that code — the client's own progress, not a clock (convention 14).
        Raises `TimeoutError` naming what the fake saw."""
        deadline = time.monotonic() + timeout
        with self._code_issued:
            while len(self._codes) < count:
                left = deadline - time.monotonic()
                if left <= 0:
                    raise TimeoutError(
                        f"the app asked for {len(self._codes)} Microsoft sign-in "
                        f"code(s), not {count}; device-code requests seen: "
                        f"{self.device_code_requests!r}"
                    )
                self._code_issued.wait(left)
            return list(self._codes)[count - 1]

    def approve(self, user_code: str) -> None:
        """The person signed in: the next poll for `user_code` gets the token."""
        self._end(user_code, _APPROVED)

    def decline(self, user_code: str) -> None:
        self._end(user_code, "authorization_declined")

    def expire(self, user_code: str) -> None:
        self._end(user_code, "expired_token")

    def refuse_consent(self, user_code: str) -> None:
        """The tenant needs its administrator's consent (`AADSTS65001`)."""
        self._end(user_code, "consent_required")

    def _end(self, user_code: str, outcome: str) -> None:
        with self._lock:
            self._codes[user_code].outcome = outcome

    # ── internals ─────────────────────────────────────────────────────

    def _answer(self, path: str, form: dict[str, str]) -> tuple[int, dict]:
        route = urllib.parse.urlsplit(path).path
        if route.endswith("/devicecode"):
            return self._devicecode(form)
        if route.endswith("/token"):
            return self._token(form)
        return 404, {"error": "invalid_request", "error_description": f"no route {route}"}

    def _devicecode(self, form: dict[str, str]) -> tuple[int, dict]:
        code = _Code(
            # Short and distinctive, as Microsoft's are; letters a person can type.
            user_code=f"FA{secrets.token_hex(3).upper()}",
            device_code=secrets.token_urlsafe(24),
        )
        with self._code_issued:
            self.device_code_requests.append(dict(form))
            self._codes[code.user_code] = code
            self._by_device[code.device_code] = code
            self._code_issued.notify_all()
        return 200, {
            "user_code": code.user_code,
            "device_code": code.device_code,
            "verification_uri": VERIFICATION_URI,
            "expires_in": self.expires_in,
            "interval": self.interval,
            "message": f"Go to {VERIFICATION_URI} and enter {code.user_code}.",
        }

    def _token(self, form: dict[str, str]) -> tuple[int, dict]:
        grant = form.get("grant_type", "")
        with self._lock:
            self.token_requests.append(
                {k: v for k, v in form.items() if k not in ("device_code", "refresh_token")}
            )
            if grant == "refresh_token":
                if form.get("refresh_token") != self.refresh_token:
                    return 400, {"error": "invalid_grant", "error_description": "unknown refresh token"}
                return 200, self._grant()
            if grant != DEVICE_CODE_GRANT:
                return 400, {"error": "unsupported_grant_type"}
            code = self._by_device.get(form.get("device_code", ""))
            if code is None:
                return 400, {"error": "invalid_grant", "error_description": "unknown device code"}
            if code.outcome == _PENDING:
                return 400, {"error": "authorization_pending"}
            if code.outcome == _APPROVED:
                # A device code is redeemed once.
                del self._by_device[code.device_code]
                return 200, self._grant()
            if code.outcome == "consent_required":
                return 400, {
                    "error": "invalid_grant",
                    "error_description": "AADSTS65001: The user or administrator has not consented.",
                    "error_codes": [65001],
                }
            return 400, {"error": code.outcome}

    def _grant(self) -> dict:
        self.issued_tokens.append(self.access_token)
        return {
            "token_type": "Bearer",
            "scope": "https://outlook.office.com/IMAP.AccessAsUser.All",
            "expires_in": self.token_lifetime,
            "access_token": self.access_token,
            "refresh_token": self.refresh_token,
        }


_SESSION: FakeMicrosoftIdentity | None = None


def session_microsoft_identity() -> FakeMicrosoftIdentity:
    """The pytest session's one identity fake, started on first use.

    The app reads `FAUNA_E2E_MICROSOFT_AUTHORITY` out of its launch
    environment and the app drivers are session-scoped (`conftest._driver_cache`)
    — so the authority, port included, is fixed before the first app starts,
    exactly as the source-IMAP trust anchor is (`fake_imap_source._session_chain`).
    It serves the session chain, which that same anchor already makes the app
    trust. Codes accumulate across tests: a test reads the code it caused with
    ``wait_for_code(count=len(fake.issued_codes) + 1)`` taken before it pressed
    the button.
    """
    global _SESSION
    if _SESSION is None:
        import atexit

        from fake_imap_source import session_cert

        from drivers.port_util import find_free_port
        from helpers import android_venue

        # The port is leased through the seam every nest port comes from: an
        # android app reaches the authority only as its own loopback through an
        # `adb reverse`, which the venue carries home for that range alone.
        port, release = android_venue.nest_port(find_free_port)
        atexit.register(release)
        _SESSION = FakeMicrosoftIdentity(cert=session_cert(), port=port).start()
    return _SESSION
