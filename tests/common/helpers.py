"""Miscellaneous helpers for Fauna E2E tests."""

import json
import os
import urllib.request

from common.envelope import FEDERATION_HELLO_V1, sign_dagcbor_envelope


def remote_request(nest_url: str, method: str, path: str, body=None, headers=None):
    """Make an HTTP request to a remote nest."""
    url = f"{nest_url}{path}"
    data = json.dumps(body).encode() if body is not None else None
    all_headers = {"Content-Type": "application/json"}
    if headers:
        all_headers.update(headers)
    req = urllib.request.Request(url, data=data, headers=all_headers, method=method)
    return urllib.request.urlopen(req)


# `pair_nests` (the peer handshake `POST /api/v1/pair`) was RETIRED by the
# per-user-pairing reshape (2026-05-25). Pairing is now an owner-scoped user
# bearer action — create it via `tests.api.ws_api.add_pairing` (`fauna.pair.add`)
# instead. The nest↔nest *sync* helper (`sign_as_nest`, below) is unchanged.


def sign_as_nest(nest_info, payload_dict):
    """Sign a JSON payload with the nest's Ed25519 key (sign-over-CID).

    Returns the hex 100-byte ``CID || sig`` envelope the federation handshake
    expects in its ``envelope`` field (``bins/fauna-nest/src/federation_sig.rs``
    `verify_payload` re-derives the CID over the canonical-dag-cbor payload and
    verifies the Ed25519 signature over ``FEDERATION_HELLO_V1 || cid``). The nest
    signs with its identity keypair, whose public half is the ``nest_id`` the
    caller passes in ``payload_dict``.

    The signature is **domain-separated** with :data:`FEDERATION_HELLO_V1`
 — the deployment key is the nest's single identity and
    signs in several contexts, so the federation hello carries a constant tag to
    keep its signatures structurally un-reinterpretable elsewhere. This is the
    Python half of the lockstep change with ``federation_sig.rs`` `sign_payload`.
    """
    from nacl.signing import SigningKey as NaClSigningKey
    # The single-identity unification retired `nest_identity.key`; the nest signs
    # with its ONE identity — the deployment seed in `nest_deployment.key`
    # (`box-recovery.md` § Single-identity unification).
    from common.nest import NEST_DEPLOYMENT_KEY

    nest_key_path = os.path.join(nest_info["tmp_dir"], NEST_DEPLOYMENT_KEY)
    with open(nest_key_path, "rb") as f:
        key_bytes = f.read()
    sk = NaClSigningKey(key_bytes)
    return sign_dagcbor_envelope(payload_dict, sk, domain_tag=FEDERATION_HELLO_V1)
