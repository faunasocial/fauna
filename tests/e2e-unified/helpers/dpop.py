"""Mint DPoP proofs (RFC 9449) for tests that drive an OAuth endpoint directly.

The nest's authorization-server endpoints are DPoP-protected: a caller must
present a proof signed by a key it holds, over the endpoint's own URL, carrying
a nonce this server issued. That is three properties an ``urllib`` request
cannot fake, so a test that wants to reach anything *past* the gate has to sign
for real — which is the point. The gate is the endpoint's first decision, and a
test that stubbed it would be testing a server that does not exist.

Deliberately a helper and not a fixture: the token and revocation endpoints
will mint proofs of their own shape (different ``htu``, and an ``ath`` where an
access token accompanies the request), and one function with named arguments is
what lets them share this without a fixture hierarchy.

⚠ **The ``htu`` is the URL the server ADVERTISES, never the one the test
dials.** A nest claimed onto ``example.test`` and reached on
``https://127.0.0.1:9000`` checks proofs against
``https://example.test/oauth/par``, because that is what its discovery document
names and what a conformant client would therefore prove over. Passing the
dialled URL here is the mistake this note exists to prevent — and the server
refusing it is a feature, not a bug: a caller reaching the nest under some other
name must not be able to satisfy the check against a URL nobody published.
"""

from __future__ import annotations

import base64
import json
import secrets
import time

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec, utils


def _b64(raw: bytes) -> str:
    """base64url, no padding — the only encoding JOSE uses."""
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


class DpopKey:
    """A P-256 key pair and the JWK the proofs made from it carry."""

    def __init__(self) -> None:
        self._private = ec.generate_private_key(ec.SECP256R1())
        numbers = self._private.public_key().public_numbers()
        self.jwk = {
            "kty": "EC",
            "crv": "P-256",
            "x": _b64(numbers.x.to_bytes(32, "big")),
            "y": _b64(numbers.y.to_bytes(32, "big")),
        }

    def sign(self, signing_input: str) -> str:
        """ES256 over ``signing_input``, as the raw ``r || s`` JOSE wants.

        ``cryptography`` produces a DER-encoded signature; JOSE wants the two
        32-byte coordinates concatenated, so the DER is decoded and re-emitted
        fixed-width. A short ``r`` or ``s`` must be LEFT-padded — dropping the
        pad is the classic intermittent-failure bug here, since it only bites
        when a coordinate happens to be small.
        """
        der = self._private.sign(signing_input.encode(), ec.ECDSA(hashes.SHA256()))
        r, s = utils.decode_dss_signature(der)
        return _b64(r.to_bytes(32, "big") + s.to_bytes(32, "big"))


def make_proof(
    key: DpopKey,
    *,
    htm: str,
    htu: str,
    nonce: str,
    jti: str | None = None,
    iat: int | None = None,
    access_token: str | None = None,
) -> str:
    """A compact DPoP proof for one request.

    ``jti`` defaults to a fresh random identifier, because a proof is
    single-use: reusing one is a replay and the server refuses it. A test that
    *wants* to prove that refusal passes the same ``jti`` twice — and the
    default makes every other test immune to accidentally doing so.

    ``access_token`` makes it a resource-server proof (RFC 9449 §4.3): the
    proof then carries ``ath``, the base64url SHA-256 of that token — required
    by a resource endpoint, refused by an authorization-server one.
    """
    if jti is None:
        jti = secrets.token_urlsafe(16)
    header = {"typ": "dpop+jwt", "alg": "ES256", "jwk": key.jwk}
    claims = {
        "jti": jti,
        "htm": htm,
        "htu": htu,
        "iat": int(time.time()) if iat is None else iat,
        "nonce": nonce,
    }
    if access_token is not None:
        digest = hashes.Hash(hashes.SHA256())
        digest.update(access_token.encode())
        claims["ath"] = _b64(digest.finalize())
    signing_input = "{}.{}".format(
        _b64(json.dumps(header, separators=(",", ":")).encode()),
        _b64(json.dumps(claims, separators=(",", ":")).encode()),
    )
    return f"{signing_input}.{key.sign(signing_input)}"
