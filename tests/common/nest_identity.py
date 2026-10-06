"""The identity every login signature binds — the Python twin of
``fauna_client_core::nest_trust::read_login_binding`` (``login.md`` § Binding
the nest).

Every bearer mint the harness makes — ``common.auth.mint_token_via_handshake``,
``WsRpcAdminClient._mint_bearer``, the tests that drive ``fauna.auth.verify`` by
hand — signs a message naming the nest it is addressed to, and the nest refuses
any other. So the signer first asks the nest to prove its identity over a fresh
client nonce (``fauna.auth.nest_handshake``) and verifies the proof; the harness
runs over plaintext loopback, so the read is possession-only (no served-cert
SPKI to compare), exactly as the wasm arm is.
"""

from __future__ import annotations

import secrets


def read_nest_identity(anon) -> bytes:
    """Return the 32-byte identity the nest at the far end of ``anon`` (a
    ``WsRpcAnonClient``) proves it holds the key for — the ``nest_id`` a login
    signature on this connection binds. Raises when the nest proves nothing or
    the proof does not verify: a login must never be signed over an identity
    the box did not prove."""
    from nacl.signing import VerifyKey

    from common.sig_domain import cert_binding_signed_message

    client_nonce = secrets.token_bytes(32)
    reply = anon.call("fauna.auth.nest_handshake", {"client_nonce": client_nonce})
    binding = reply.get("cert_binding")
    if not binding:
        raise RuntimeError(
            f"fauna.auth.nest_handshake proved no identity (reply {reply!r}) — "
            f"nothing for a login signature to bind"
        )
    nest_id = bytes.fromhex(binding["nest_actor_id"])
    tagged_sig = binding.get("tagged_sig")
    if not tagged_sig:
        raise RuntimeError("nest handshake binding carries no tagged signature")
    spki = bytes(binding.get("spki_sha256") or b"")
    VerifyKey(nest_id).verify(cert_binding_signed_message(spki, client_nonce), bytes(tagged_sig))
    return nest_id
