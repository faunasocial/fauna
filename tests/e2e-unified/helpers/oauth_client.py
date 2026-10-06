"""A minimal external ATProto **OAuth client**, for the F4 consent-ceremony
tier_3 leg.

This stands in for the third-party app the whole F4 chain exists to serve — a
Graysky or an Ivory pointed at a Fauna nest. It speaks the parts of the flow the
consent slice needs and no more: a DPoP proof, a pushed authorization request,
the browser's GET of the consent page, and that page's long-poll.

**Why a hand-rolled client rather than a library.** The property under test is
that OUR two consent surfaces agree — the code the browser page renders and the
code the user's own app renders come from one nest-minted value — and that the
approval in the app is what releases the browser's authorization code. A library
would hide exactly the wire this test exists to walk. It is also deliberately
NOT a conformance client: the reference `@atproto/oauth-client-*` acceptance run
is F5's job (`atproto-pds-full.md` § F4 detail → Acceptance test).

The **loopback development client** (`http://localhost?redirect_uri=…`) is used
on purpose: its metadata document is synthesized from its own `client_id` with
no fetch at all, so this test needs no HTTPS-servable document — and the carve-
out's own security boundary (every declared `redirect_uri` on 127.0.0.1/[::1])
is what makes that identity safe to hand out.
"""

from __future__ import annotations

import base64
import hashlib
import json
import re
import secrets
import ssl
import time
import urllib.error
import urllib.parse
import urllib.request

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec, utils as asym_utils

# The bridge terminates its own `pds.<domain>` TLS with the nest's self-signed
# floor cert at tier_3, and the property under test is never the cert chain
# (that is tier_4 `test_atproto_pds_sni_router.py`).
TLS_INSECURE = ssl._create_unverified_context()

PAR_PATH = "/oauth/par"
AUTHORIZE_PATH = "/oauth/authorize"
AUTHORIZE_POLL_PATH = "/oauth/authorize/poll"
TOKEN_PATH = "/oauth/token"
REVOKE_PATH = "/oauth/revoke"
DEVICE_AUTHORIZATION_PATH = "/oauth/device_authorization"
BC_AUTHORIZE_PATH = "/oauth/bc-authorize"
GRANT_TYPE_DEVICE_CODE = "urn:ietf:params:oauth:grant-type:device_code"
GRANT_TYPE_CIBA = "urn:openid:params:grant-type:ciba"
GRANT_TYPE_HANDOFF = "urn:fauna:params:grant-type:handoff"

# Lowercase, and read out of a lowercased header map — HTTP field names are
# case-insensitive and Go canonicalizes this one to `Dpop-Nonce`, so a client
# matching the spec's `DPoP-Nonce` spelling exactly finds nothing and can never
# complete the round trip the server is telling it to make. (Cost this test one
# full four-process run to learn.)
NONCE_HEADER = "dpop-nonce"


def _b64u(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def _json_or_raw(body: str) -> dict:
    try:
        parsed = json.loads(body)
    except json.JSONDecodeError:
        return {"raw": body}
    return parsed if isinstance(parsed, dict) else {"raw": body}


def _b64u_json(obj) -> str:
    return _b64u(json.dumps(obj, separators=(",", ":")).encode())


class OAuthClient:
    """One external app's OAuth identity: an ES256 DPoP key plus PKCE state."""

    def __init__(
        self,
        base: str,
        htu_origin: str,
        redirect_uri: str,
        scope: str = "atproto",
        resource_base: str | None = None,
        resource_htu_origin: str | None = None,
        holder_x25519: bytes | None = None,
        request_scope: str | None = None,
        writer_ed25519: bytes | None = None,
        client_id_url: str | None = None,
        assertion_key: ec.EllipticCurvePrivateKey | None = None,
        assertion_kid: str | None = None,
        issuer: str | None = None,
    ):
        # `base` is where this test DIALS (loopback). `htu_origin` is the origin
        # the server publishes itself at, which is what a DPoP proof's `htu`
        # must name — the two differ whenever the server derives its host from a
        # primary domain, and conflating them is the single most likely way this
        # test fails for a reason that is not a product bug.
        self.base = base.rstrip("/")
        self.htu_origin = htu_origin.rstrip("/")
        # The RESOURCE server — the PDS, on its own host — is a different server
        # from the authorization server (the nest) once the issuer moved, so it
        # has its own dial address, its own published origin, and its own DPoP
        # nonce. Absent, it is the same server as `base`.
        self.resource_base = (resource_base or base).rstrip("/")
        self.resource_htu_origin = (resource_htu_origin or htu_origin).rstrip("/")
        self.resource_nonce: str | None = None
        self.redirect_uri = redirect_uri
        # The scope string this client both DECLARES (in its loopback
        # `client_id`, which is its metadata document) and REQUESTS at PAR —
        # one value, because a request for a scope the metadata does not
        # declare is refused, and a test that drifted the two apart would be
        # asserting that refusal by accident.
        self.scope = scope
        # What one PAR REQUESTS, when narrower than what the document declares
        # — a re-consent for a subset (`declared_scopes` bounds a request from
        # above, never from below). Absent, a ceremony requests the whole
        # declared set, which is the default every caller above relies on.
        self.request_scope = request_scope or scope
        self.key =ec.generate_private_key(ec.SECP256R1())
        self.nonce: str | None = None
        self.verifier = _b64u(secrets.token_bytes(32))
        self.challenge = _b64u(hashlib.sha256(self.verifier.encode()).digest())
        self.state = _b64u(secrets.token_bytes(16))
        # The X25519 public key this client attests as its capability-grant
        # holder (`third-party.md` § The principal model, rule 2) — pushed at
        # PAR as `fauna_holder_x25519`, so the DPoP key that proved the push is
        # the one that must redeem the code the attestation rides to. Absent,
        # the client is a standard OAuth client that presents none.
        self.holder_x25519 = holder_x25519
        # The Ed25519 writer key this client attests beside the holder key
        # (`third-party-kinds.md` § Principal write authority) — pushed at PAR
        # as `fauna_writer_ed25519`. Absent, the client is read-only over its
        # kinds.
        self.writer_ed25519 = writer_ed25519
        # An `https` client_id — a fetched metadata document the caller serves
        # (`helpers.client_metadata_server`) — in place of the loopback dev
        # client. Its document, not this object, declares the scopes, so
        # `scope` must agree with it.
        self.client_id_url = client_id_url
        # A CONFIDENTIAL client (`private_key_jwt`, RFC 7523 §2.2): the P-256
        # key its document's `jwks` declares under `assertion_kid`, and the
        # issuer identifier an assertion's `aud` must name. Absent, the client
        # is public. A confidential client consents as a *remote*-form
        # principal (`third-party.md` § The principal model, rule 3).
        self.assertion_key = assertion_key
        self.assertion_kid = assertion_kid
        self.issuer = issuer

    def assertion_jwk(self) -> dict:
        """The public half of the assertion key, as the document's `jwks`
        member lists it."""
        nums = self.assertion_key.public_key().public_numbers()
        return {
            "kty": "EC",
            "crv": "P-256",
            "use": "sig",
            "kid": self.assertion_kid,
            "x": _b64u(nums.x.to_bytes(32, "big")),
            "y": _b64u(nums.y.to_bytes(32, "big")),
        }

    def _client_assertion(self) -> str:
        """A fresh `private_key_jwt` assertion — fresh per request, since the
        server keeps a `jti` replay set."""
        now = int(time.time())
        header = {"alg": "ES256", "kid": self.assertion_kid}
        claims = {
            "iss": self.client_id,
            "sub": self.client_id,
            "aud": self.issuer,
            "jti": _b64u(secrets.token_bytes(16)),
            "iat": now,
            "exp": now + 60,
        }
        signing_input = f"{_b64u_json(header)}.{_b64u_json(claims)}".encode()
        der = self.assertion_key.sign(signing_input, ec.ECDSA(hashes.SHA256()))
        r, s = asym_utils.decode_dss_signature(der)
        raw = r.to_bytes(32, "big") + s.to_bytes(32, "big")
        return f"{signing_input.decode()}.{_b64u(raw)}"

    def _authenticated(self, form: dict) -> dict:
        """`form` plus this client's assertion, when it is confidential."""
        if self.assertion_key is None:
            return form
        return {
            **form,
            "client_assertion_type": "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
            "client_assertion": self._client_assertion(),
        }

    @property
    def client_id(self) -> str:
        """The `https` document URL when one was given; otherwise the loopback
        dev client's `client_id` — which IS its metadata document, hence the
        redirect_uri riding in the query string."""
        if self.client_id_url:
            return self.client_id_url
        q = urllib.parse.urlencode({"redirect_uri": self.redirect_uri, "scope": self.scope})
        return f"http://localhost?{q}"

    # ── DPoP ────────────────────────────────────────────────────────────
    def _jwk(self) -> dict:
        nums = self.key.public_key().public_numbers()
        return {
            "kty": "EC",
            "crv": "P-256",
            "x": _b64u(nums.x.to_bytes(32, "big")),
            "y": _b64u(nums.y.to_bytes(32, "big")),
        }

    def dpop_proof(self, htm: str, htu: str, access_token: str | None = None) -> str:
        """A DPoP proof over one request.

        `access_token` switches the proof from the authorization-server shape to
        the **resource-server** one: it adds `ath`, the presented token's own
        hash, which is what stops a proof captured alongside one token being
        replayed alongside another (RFC 9449 §4.3). An AS endpoint takes no
        token and *refuses* a proof carrying `ath`, so this stays absent there.
        """
        header = {"typ": "dpop+jwt", "alg": "ES256", "jwk": self._jwk()}
        claims = {
            "jti": _b64u(secrets.token_bytes(16)),
            "htm": htm,
            "htu": htu,
            "iat": int(time.time()),
        }
        if access_token:
            claims["ath"] = _b64u(hashlib.sha256(access_token.encode()).digest())
        if self.nonce:
            claims["nonce"] = self.nonce
        signing_input = f"{_b64u_json(header)}.{_b64u_json(claims)}".encode()
        der = self.key.sign(signing_input, ec.ECDSA(hashes.SHA256()))
        r, s = asym_utils.decode_dss_signature(der)
        raw = r.to_bytes(32, "big") + s.to_bytes(32, "big")
        return f"{signing_input.decode()}.{_b64u(raw)}"

    # ── The flow ────────────────────────────────────────────────────────
    def push_authorization_request(self, login_hint: str | None = None) -> str:
        """POST /oauth/par → the `request_uri` handle.

        Runs the nonce round trip first, which is not a workaround: this server
        supplies nonces on every response including refusals, precisely so a
        client that has never spoken to it recovers from the response that told
        it so. A correct client's FIRST request always fails this way.
        """
        status, body = self._push_authorization_request(login_hint)
        assert status == 201, f"PAR refused after the nonce round trip: {status} {body!r}"
        return body["request_uri"]

    def push_authorization_request_expecting_refusal(
        self, login_hint: str | None = None
    ) -> tuple[int, dict]:
        """A PAR the caller expects to FAIL, with the parsed body.

        Separate from [push_authorization_request] for the reason
        [token_request_expecting_refusal] is separate from the token request:
        a negative test must not pass by accidentally succeeding, so this one
        asserts nothing about the status and hands it back to be judged.
        """
        return self._push_authorization_request(login_hint)

    def _push_authorization_request(self, login_hint: str | None) -> tuple[int, dict]:
        form = {
            "client_id": self.client_id,
            "response_type": "code",
            "redirect_uri": self.redirect_uri,
            "scope": self.request_scope,
            "state": self.state,
            "code_challenge": self.challenge,
            "code_challenge_method": "S256",
        }
        if login_hint:
            form["login_hint"] = login_hint
        if self.holder_x25519 is not None:
            form["fauna_holder_x25519"] = _b64u(self.holder_x25519)
        if self.writer_ed25519 is not None:
            form["fauna_writer_ed25519"] = _b64u(self.writer_ed25519)

        htu = f"{self.htu_origin}{PAR_PATH}"
        # First attempt: no nonce → refused, and the refusal carries one.
        status, body, headers = self._post_form(PAR_PATH, self._authenticated(form), htu)
        if status != 201:
            self.nonce = headers.get(NONCE_HEADER)
            assert self.nonce, (
                f"PAR refused {status} without handing back a DPoP-Nonce, so no client "
                f"could ever recover: {body!r}"
            )
            status, body, headers = self._post_form(PAR_PATH, self._authenticated(form), htu)
        if headers.get(NONCE_HEADER):
            self.nonce = headers[NONCE_HEADER]
        try:
            return status, json.loads(body)
        except json.JSONDecodeError:
            return status, {"raw": body}

    def open_consent_page(self, request_uri: str) -> tuple[str, str]:
        """GET /oauth/authorize — the browser's step. Returns
        `(binding_code, flow_token)` scraped from the rendered page.

        The page is the browser's whole surface: it shows the client identity,
        the human-readable scopes and the binding code, and its own script
        long-polls for the answer. This reads the two values a test needs — the
        code (to compare against the app's card) and the flow token (the poll's
        only credential).
        """
        q = urllib.parse.urlencode({"client_id": self.client_id, "request_uri": request_uri})
        req = urllib.request.Request(f"{self.base}{AUTHORIZE_PATH}?{q}", method="GET")
        status, html, _ = self._do(req)
        assert status == 200, f"consent page refused: {status}\n{html[:800]}"
        code = re.search(r'id="binding-code">([^<]+)<', html)
        flow = re.search(r'data-flow="([^"]+)"', html)
        assert code and flow, f"consent page did not render a code + flow token:\n{html[:1200]}"
        return code.group(1).strip(), flow.group(1)

    def poll(self, flow_token: str) -> tuple[str, str]:
        """POST /oauth/authorize/poll → `(status, redirect)`.

        The poll HOLDS while the request is pending — that is the mechanism, not
        a delay to work around — so the caller decides its own budget by how
        many times it is willing to call this.
        """
        status, body, _ = self._post_form(
            AUTHORIZE_POLL_PATH, {"flow": flow_token}, htu=None
        )
        answer = json.loads(body)
        return answer.get("status", ""), answer.get("redirect", "")

    # ── The polled consent starts (TP9) ─────────────────────────────────
    def start_polled(self, path: str, form: dict) -> tuple[int, dict]:
        """POST one of the polled consent starts — ``/oauth/device_authorization``
        (the typed code) or ``/oauth/bc-authorize`` (the quiet push) — and return
        ``(status, parsed-body)``, with the DPoP nonce round trip every AS
        endpoint here makes. The client's own ``client_id`` and ``scope`` are
        filled in; ``form`` adds the start's own parameters.
        """
        full = {"client_id": self.client_id, "scope": self.scope, **form}
        htu = f"{self.htu_origin}{path}"
        status, body, headers = self._post_form(path, full, htu)
        if status != 200 and headers.get(NONCE_HEADER):
            self.nonce = headers[NONCE_HEADER]
            status, body, headers = self._post_form(path, full, htu)
        if headers.get(NONCE_HEADER):
            self.nonce = headers[NONCE_HEADER]
        try:
            return status, json.loads(body)
        except json.JSONDecodeError:
            return status, {"raw": body}

    def poll_token(
        self, grant_type: str, handle_param: str, handle: str, extra: dict | None = None
    ) -> tuple[int, dict]:
        """One poll of a polled start at ``/oauth/token`` — ``(status, body)``.

        ``extra`` adds grant-specific parameters — the same-device handoff's
        ``code_verifier``, since its request began with PAR.

        Returns rather than asserts: ``authorization_pending`` and ``slow_down``
        are answers here, not failures, and the caller judges which it expected.

        ⚠ Retries ONLY on a nonce refusal, never on any other non-200 the way
        [token_request_expecting_refusal] does: a pending poll is a refusal that
        carries a nonce too, and re-sending it at once would be a second poll
        inside the interval — the client turning its own ``authorization_pending``
        into ``slow_down``.
        """
        form = {
            "grant_type": grant_type,
            handle_param: handle,
            "client_id": self.client_id,
            **(extra or {}),
        }
        htu = f"{self.htu_origin}{TOKEN_PATH}"
        status, body, headers = self._post_form(TOKEN_PATH, form, htu)
        parsed = _json_or_raw(body)
        if (
            status != 200
            and parsed.get("error") in ("use_dpop_nonce", "invalid_dpop_proof")
            and headers.get(NONCE_HEADER)
        ):
            self.nonce = headers[NONCE_HEADER]
            status, body, headers = self._post_form(TOKEN_PATH, form, htu)
            parsed = _json_or_raw(body)
        if headers.get(NONCE_HEADER):
            self.nonce = headers[NONCE_HEADER]
        return status, parsed

    # ── The token exchange (F4 slice 7) ─────────────────────────────────
    def exchange_code(self, redirect: str) -> dict:
        """Redeem the released authorization code at POST /oauth/token.

        Takes the whole redirect URL rather than a bare code on purpose: the
        code is only ever *delivered* inside one, so parsing it here is the
        client's own job and a test that passed a code around would be skipping
        the step where a real client can get it wrong.
        """
        code = urllib.parse.parse_qs(urllib.parse.urlparse(redirect).query).get("code", [""])[0]
        assert code, f"no code in the released redirect: {redirect}"
        form = {
            "grant_type": "authorization_code",
            "code": code,
            "client_id": self.client_id,
            "redirect_uri": self.redirect_uri,
            "code_verifier": self.verifier,
        }
        return self._token_request(form)

    def refresh(self, refresh_token: str) -> dict:
        """Rotate the grant's token pair at POST /oauth/token."""
        return self._token_request(
            {"grant_type": "refresh_token", "refresh_token": refresh_token}
        )

    def _token_request(self, form: dict) -> dict:
        htu = f"{self.htu_origin}{TOKEN_PATH}"
        status, body, headers = self._post_form(TOKEN_PATH, form, htu)
        # The same nonce round trip PAR makes: this client's nonce may have
        # aged out between the consent ceremony and here, and the refusal
        # carries a fresh one.
        if status != 200 and headers.get(NONCE_HEADER):
            self.nonce = headers[NONCE_HEADER]
            status, body, headers = self._post_form(TOKEN_PATH, form, htu)
        assert status == 200, f"token exchange refused: {status} {body!r}"
        if headers.get(NONCE_HEADER):
            self.nonce = headers[NONCE_HEADER]
        return json.loads(body)

    def revoke(self, token: str, token_type_hint: str | None = None) -> tuple[int, str]:
        """Sign out at POST /oauth/revoke (RFC 7009), returning (status, body).

        Returns the status rather than asserting it, because this endpoint's
        whole design is that **almost every outcome is 200** — revoked, unknown,
        expired, other-plane, bound to a different key — so a helper that
        asserted success would be asserting nothing. What a caller checks is the
        *effect*: the grant it revoked stops rotating.

        `token_type_hint` is passed through when given so a test can prove the
        hint ORDERS the search without limiting it (RFC 7009 §2.1) — a client
        that mislabels its own token must still get it revoked.
        """
        htu = f"{self.htu_origin}{REVOKE_PATH}"
        form = {"token": token}
        if token_type_hint:
            form["token_type_hint"] = token_type_hint
        status, body, headers = self._post_form(REVOKE_PATH, form, htu)
        # The same nonce round trip every other endpoint makes: this client's
        # nonce may have aged out since its last request, and the refusal that
        # says so carries a fresh one.
        if status != 200 and headers.get(NONCE_HEADER):
            self.nonce = headers[NONCE_HEADER]
            status, body, headers = self._post_form(REVOKE_PATH, form, htu)
        if headers.get(NONCE_HEADER):
            self.nonce = headers[NONCE_HEADER]
        return status, body

    def token_request_expecting_refusal(self, form: dict) -> tuple[int, dict]:
        """A token request the caller expects to FAIL, with the parsed body.

        Kept separate from [_token_request] so a negative test cannot pass by
        accidentally succeeding: this one asserts nothing about the status and
        hands it back for the test to judge.
        """
        htu = f"{self.htu_origin}{TOKEN_PATH}"
        status, body, headers = self._post_form(TOKEN_PATH, form, htu)
        if status != 200 and headers.get(NONCE_HEADER):
            self.nonce = headers[NONCE_HEADER]
            status, body, headers = self._post_form(TOKEN_PATH, form, htu)
            if headers.get(NONCE_HEADER):
                self.nonce = headers[NONCE_HEADER]
        try:
            return status, json.loads(body)
        except json.JSONDecodeError:
            return status, {"raw": body}

    def authed_xrpc_get(self, nsid: str, access_token: str, **params) -> tuple[int, dict]:
        """An authenticated XRPC GET the way a real OAuth client makes one.

        `Authorization: DPoP <token>` — never `Bearer`. The scheme is what
        selects the plane server-side, and presenting a bound token as a bearer
        credential is refused by construction, so a test using `Bearer` here
        would be exercising a path this design deliberately closed.

        **The nonce is re-harvested from EVERY response, success included** —
        this plane issues one on all of them,
        exactly as the authorization-server endpoints do. Harvesting only on
        refusals is what let the defect hide: this client kept one nonce taken
        from an AS response and reused it for resource-server calls, which is
        correct only inside the 4-minute freshness window every test happened to
        finish within. A real client outlives that window, and its nonce has to
        come from the last response it actually received — from the RESOURCE
        server, which mints its own, never the authorization server's.
        """
        path = f"/xrpc/{nsid}"
        if params:
            path = f"{path}?{urllib.parse.urlencode(params)}"
        # `htu` has no query string (RFC 9449 §4.3), and it names the PDS's own
        # published origin rather than the host this client happened to dial.
        htu = f"{self.resource_htu_origin}/xrpc/{nsid}"
        # `dpop_proof` binds `self.nonce`; hold the authorization server's
        # nonce aside while proving to the resource server, so neither
        # server's nonce is ever presented to the other.
        as_nonce, self.nonce = self.nonce, self.resource_nonce
        try:
            req = urllib.request.Request(f"{self.resource_base}{path}", method="GET")
            req.add_header("Authorization", f"DPoP {access_token}")
            req.add_header("DPoP", self.dpop_proof("GET", htu, access_token=access_token))
            status, body, headers = self._do(req)
            if status != 200 and headers.get(NONCE_HEADER):
                self.nonce = headers[NONCE_HEADER]
                req = urllib.request.Request(f"{self.resource_base}{path}", method="GET")
                req.add_header("Authorization", f"DPoP {access_token}")
                req.add_header("DPoP", self.dpop_proof("GET", htu, access_token=access_token))
                status, body, headers = self._do(req)
            if headers.get(NONCE_HEADER):
                self.nonce = headers[NONCE_HEADER]
        finally:
            self.resource_nonce, self.nonce = self.nonce, as_nonce
        try:
            return status, json.loads(body)
        except json.JSONDecodeError:
            return status, {"raw": body}

    # ── transport ───────────────────────────────────────────────────────
    def _post_form(self, path: str, form: dict, htu: str | None):
        data = urllib.parse.urlencode(form).encode()
        req = urllib.request.Request(
            f"{self.base}{path}",
            data=data,
            method="POST",
            headers={"Content-Type": "application/x-www-form-urlencoded"},
        )
        if htu:
            req.add_header("DPoP", self.dpop_proof("POST", htu))
        return self._do(req)

    @staticmethod
    def _do(req):
        def lower(headers) -> dict:
            return {k.lower(): v for k, v in headers.items()}

        try:
            with urllib.request.urlopen(req, timeout=30, context=TLS_INSECURE) as r:
                return r.status, r.read().decode(errors="replace"), lower(r.headers)
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode(errors="replace"), lower(e.headers)
