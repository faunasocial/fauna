"""tier_3 API: a sign-in made for one nest is refused by every other nest.

``docs/features/connect-and-sign-in.md`` outcome **18**, observed from outside
on two real nests. Owner: ``docs/goal/behavior/login.md`` § Two auth endpoints
→ *Binding the nest* — every bearer-minting signature names the receiving
nest's identity; a nest requires the request's ``nest_id`` to be its own and
refuses any other with ``fauna.auth.invalid_request`` before any signature
work, and it verifies over its OWN identity, so the same blob re-addressed to
it fails its signature. No unbound form is accepted.

The fixture is the shape the ruling guards against: ONE identity known on two
nests (``linked-nests.md`` — the user's one key, registered on both), a sign-in
signed for nest A, and nest A — or anyone holding the blob — presenting it to
nest B. Each mint kind is witnessed with its control: the blob addressed to A
mints on A (or, for the silent challenge, the genuine B-addressed verify mints
on B after both refusals), so the refusals cannot pass on a nest that refuses
everything. The in-process pins are ``conformance_auth.rs``
(``handshake_addressed_to_another_nest_is_refused``,
``verify_addressed_to_another_nest_is_refused``) and ``auth_core.rs``
(``a_device_handshake_addressed_to_another_nest_is_refused``); this file is
their outside-observed twin across all four kinds.

The custody handshake's control needs the custody capability row on the nest
it is made for: the harness mints it on nest A through the shared custody act
(``fauna_ffi.harness_mint_custody``), the way the ceremony does.

Latency-independent (convention 14): every step is a request and its reply.
"""

from __future__ import annotations

import os
import secrets
import time

import pytest
from nacl.signing import SigningKey

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import register_user
from common.nest_identity import read_nest_identity
from common.sig_domain import (
    challenge_verify_signed_message,
    device_handshake_signed_message,
    handshake_signed_message,
)

pytestmark = pytest.mark.tier_3

#: The refusal of a blob addressed to another nest (`login.md` § Binding the nest).
NOT_THIS_NEST = "fauna.auth.invalid_request"
#: The refusal of the same blob re-addressed: the signature named the other nest.
SIGNATURE_FAILED = "fauna.auth.signature_failed"
#: A custody window (seconds) and a witness expiry (microseconds) that never
#: close inside a run — the values `helpers/custody_hosting.py` plants.
CUSTODY_WINDOW_SECS = 90 * 24 * 3600
FAR_FUTURE_US = 9_000_000_000_000_000


# ── harness ──────────────────────────────────────────────────────────────────


def _identity(nest) -> bytes:
    with WsRpcAnonClient(nest["url"]) as anon:
        return read_nest_identity(anon)


def _admit(nest, key: SigningKey) -> dict:
    """Admit ``key`` on ``nest`` as an account — the same identity on every nest
    it is admitted to. Fixture setup."""
    actor_hex = bytes(key.verify_key).hex()
    register_user(
        nest["port"],
        actor_hex,
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
        handle=f"e2e-{actor_hex[:12]}",
    )
    return {"actor_id_hex": actor_hex, "signing_key": bytes(key)}


def _call(nest, kind: str, payload: dict) -> dict:
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call(kind, payload)


def _refused(nest, kind: str, payload: dict) -> str:
    with pytest.raises(RpcCallError) as refused:
        _call(nest, kind, payload)
    return refused.value.code


def _handshake(key: SigningKey, nest_id: bytes) -> dict:
    actor = bytes(key.verify_key)
    timestamp = int(time.time() * 1000)
    nonce = secrets.token_bytes(32)
    return {
        "actor_id": actor.hex(),
        "timestamp": timestamp,
        "signature": key.sign(handshake_signed_message(actor, timestamp, nest_id, nonce)).signature.hex(),
        "client_nonce": nonce,
        "nest_id": nest_id.hex(),
    }


def _device_handshake(owner: SigningKey, device: SigningKey, nest_id: bytes) -> dict:
    actor, device_pub = bytes(owner.verify_key), bytes(device.verify_key)
    timestamp = int(time.time() * 1000)
    nonce = secrets.token_bytes(32)
    message = device_handshake_signed_message(actor, device_pub, timestamp, nest_id, nonce)
    return {
        "actor_id": actor.hex(),
        "device_key": device_pub.hex(),
        "timestamp": timestamp,
        "signature": device.sign(message).signature.hex(),
        "client_nonce": nonce,
        "nest_id": nest_id.hex(),
    }


def _readdressed(payload: dict, nest_id: bytes) -> dict:
    return {**payload, "nest_id": nest_id.hex()}


@pytest.fixture()
def one_key_two_nests(two_nodes):
    """Two nests, one identity admitted on both, and each nest's proven id."""
    a, b = two_nodes["nest_a"], two_nodes["nest_b"]
    key = SigningKey(os.urandom(32))
    user_a, user_b = _admit(a, key), _admit(b, key)
    return {
        "a": a,
        "b": b,
        "key": key,
        "user_a": user_a,
        "user_b": user_b,
        "id_a": _identity(a),
        "id_b": _identity(b),
    }


# ── the four mint kinds ──────────────────────────────────────────────────────


@pytest.mark.feature("connect-and-sign-in")
def test_a_sign_in_handshake_made_for_one_nest_is_refused_by_another(one_key_two_nests):
    """connect-and-sign-in 18, `fauna.auth.handshake`."""
    a, b, key = one_key_two_nests["a"], one_key_two_nests["b"], one_key_two_nests["key"]
    assert one_key_two_nests["id_a"] != one_key_two_nests["id_b"], "precondition: two nests, two identities"
    blob = _handshake(key, one_key_two_nests["id_a"])

    assert _refused(b, "fauna.auth.handshake", blob) == NOT_THIS_NEST, (
        "nest B must refuse a handshake addressed to nest A, before any signature work"
    )
    assert _refused(b, "fauna.auth.handshake", _readdressed(blob, one_key_two_nests["id_b"])) == SIGNATURE_FAILED, (
        "re-addressed to nest B, the blob must still fail: its signature names nest A"
    )
    assert _call(a, "fauna.auth.handshake", blob)["token"], (
        "control: the same blob mints on the nest it was made for"
    )


@pytest.mark.feature("connect-and-sign-in")
def test_a_silent_challenge_answer_made_for_one_nest_is_refused_by_another(one_key_two_nests):
    """connect-and-sign-in 18, `fauna.auth.challenge` + `fauna.auth.verify` —
    the ceremony every app-held bearer is refreshed over: a relaying nest A
    takes nest B's challenge to the user, who answers it for A."""
    b, key = one_key_two_nests["b"], one_key_two_nests["key"]
    actor = bytes(key.verify_key)
    with WsRpcAnonClient(b["url"]) as anon:
        nonce_hex = anon.call("fauna.auth.challenge", {"actor_id": actor.hex()})["nonce"]
    nonce = bytes.fromhex(nonce_hex)

    def answer(signed_for: bytes, addressed_to: bytes) -> dict:
        signature = key.sign(challenge_verify_signed_message(actor, nonce, signed_for)).signature
        return {
            "actor_id": actor.hex(),
            "nonce": nonce_hex,
            "signature": signature.hex(),
            "nest_id": addressed_to.hex(),
        }

    id_a, id_b = one_key_two_nests["id_a"], one_key_two_nests["id_b"]
    assert _refused(b, "fauna.auth.verify", answer(id_a, id_a)) == NOT_THIS_NEST, (
        "nest B must refuse a challenge answer addressed to nest A"
    )
    assert _refused(b, "fauna.auth.verify", answer(id_a, id_b)) == SIGNATURE_FAILED, (
        "re-addressed to nest B, the answer must still fail: its signature names nest A"
    )
    assert _call(b, "fauna.auth.verify", answer(id_b, id_b))["token"], (
        "control: the genuine answer for nest B mints — the refusals consumed nothing"
    )


@pytest.mark.feature("connect-and-sign-in")
def test_a_device_sign_in_made_for_one_nest_is_refused_by_another(one_key_two_nests):
    """connect-and-sign-in 18, `fauna.auth.device_handshake` — one device key
    granted on the account at both nests."""
    from helpers.enrollment import register_granted_device

    a, b, key = one_key_two_nests["a"], one_key_two_nests["b"], one_key_two_nests["key"]
    device = SigningKey(os.urandom(32))
    for nest, user in ((a, one_key_two_nests["user_a"]), (b, one_key_two_nests["user_b"])):
        register_granted_device(nest["url"], user, "e2e-one_key_two_nests-device", principal_seed=bytes(device))
    blob = _device_handshake(key, device, one_key_two_nests["id_a"])

    assert _refused(b, "fauna.auth.device_handshake", blob) == NOT_THIS_NEST, (
        "nest B must refuse a device sign-in addressed to nest A"
    )
    assert (
        _refused(b, "fauna.auth.device_handshake", _readdressed(blob, one_key_two_nests["id_b"]))
        == SIGNATURE_FAILED
    ), "re-addressed to nest B, the device sign-in must still fail: its signature names nest A"
    assert _call(a, "fauna.auth.device_handshake", blob)["token"], (
        "control: the same device sign-in mints on the nest it was made for"
    )



@pytest.mark.feature("connect-and-sign-in")
def test_a_custody_sign_in_made_for_one_nest_is_refused_by_another(one_key_two_nests):
    """connect-and-sign-in 18, `fauna.auth.custody_handshake` — the owner's
    custody minted on nest A for a custodian key; the custodian's sign-in made
    for nest A."""
    import cbor2
    import fauna_ffi

    from common.sig_domain import custody_handshake_signed_message

    a, b, key = one_key_two_nests["a"], one_key_two_nests["b"], one_key_two_nests["key"]
    owner = bytes(key.verify_key)
    custodian = SigningKey(os.urandom(32))
    custodian_pub = bytes(custodian.verify_key)
    grant_id = os.urandom(16)
    now = int(time.time())
    minted = fauna_ffi.harness_mint_custody(
        a["url"], bytes(key), grant_id, custodian_pub, now, now + CUSTODY_WINDOW_SECS
    )
    assert minted.get("ok") is True, f"precondition: the custody row is minted on nest A: {minted}"
    witness = fauna_ffi.sign_custody_witness(bytes(key), grant_id, custodian_pub, FAR_FUTURE_US)

    id_a = one_key_two_nests["id_a"]
    timestamp = int(time.time() * 1000)
    nonce = secrets.token_bytes(32)
    blob = {
        "owner_actor_id": owner.hex(),
        "custodian_key": custodian_pub.hex(),
        "timestamp": timestamp,
        "signature": custodian.sign(
            custody_handshake_signed_message(owner, custodian_pub, timestamp, id_a, nonce)
        ).signature.hex(),
        "client_nonce": nonce,
        # The handshake carries the witness inline as its `EmbedAsBytes` map
        # ({envelope, bytes}), where the hosting door takes the canonical
        # bytes verbatim: the same value, decoded once.
        "witness": cbor2.loads(witness),
        "nest_id": id_a.hex(),
    }

    assert _refused(b, "fauna.auth.custody_handshake", blob) == NOT_THIS_NEST, (
        "nest B must refuse a custody sign-in addressed to nest A, at the binding gate"
    )
    assert _call(a, "fauna.auth.custody_handshake", blob)["token"], (
        "control: the same custody sign-in mints on the nest it was made for"
    )
