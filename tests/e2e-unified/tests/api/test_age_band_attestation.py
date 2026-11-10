"""A verified age band comes only from a valid store attestation, witnessed at
the wire (`docs/goal/behavior/account-age-band.md` § The account age band;
`docs/features/join-a-nest.md` outcome 16).

The real nest, no verifier stand-in and no trust-root swap (a deliberate
security boundary): the rule is driven from outside with attestations that
cannot verify, over the kinds a production app rides — the anonymous
`fauna.account.age_nonce` mint, then `fauna.account.register` carrying an
`age_claim` with an `attestation`.

* a nonce the nest never minted refuses the WHOLE admission with
  `fauna.account.age_attestation_invalid`;
* a nonce the nest minted is single-use: spent by the first (bad) attestation,
  a second attempt with it is refused as spent, and a fresh nonce is not
  affected by the spent one;
* an attestation naming a bundle the nest does not pin is refused for that
  reason, while one naming a pinned bundle gets past it (so the refusal is the
  bundle check, not "this call never works");
* every refusal leaves no account (the key is `not_registered` at sign-in);
* a platform the nest holds no verifier for (android while its Play Integrity
  keys are unset) is not a failed check: the admission goes through as a
  declared claim and mints no verified band row.

The valid-attestation (verified) arm is not claimed here: it needs a chain to
Apple's root, and stays pinned by the `age_attest` unit tests.
"""

from __future__ import annotations

import hashlib
import secrets
import time

import pytest
from nacl.signing import SigningKey

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import handshake_via_ws, mint_token_via_handshake, set_registration_mode
from common.sig_domain import register_signed_message

pytestmark = pytest.mark.tier_3

NEST_DOMAIN = "ageband.example"
ATTESTATION_INVALID = "fauna.account.age_attestation_invalid"
NOT_REGISTERED = "fauna.auth.not_registered"

# The App ID the nest pins for the main iOS app (`FAUNA_IOS_APP_ID`).
PINNED_IOS_APP_ID = "7457N3M72H.social.fauna.fauna"
UNPINNED_IOS_APP_ID = "ABCDE12345.social.someone.else"


def _as_signing_key(raw) -> SigningKey:
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


@pytest.fixture(scope="module")
def age_nest(request, nest_mode, tmp_path_factory):
    """A nest claimed onto ``NEST_DOMAIN`` (the domain every register signature
    is over), registration open."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "ageband-nest", claim_domain=NEST_DOMAIN,
    )
    nest["admin_sk"] = _as_signing_key(nest["admin"]["signing_key"])
    set_registration_mode(nest["port"], base_url=nest["url"],
                          admin_signing_key=nest["admin_sk"], mode="open")
    yield nest
    cleanup()


# ── minimal CBOR (definite-length, the shapes the nest's strict reader takes) ──


def _hdr(major: int, n: int) -> bytes:
    if n < 24:
        return bytes([major << 5 | n])
    assert n < 256
    return bytes([major << 5 | 24, n])


def _text(s: str) -> bytes:
    return _hdr(3, len(s)) + s.encode()


def _bstr(b: bytes) -> bytes:
    return _hdr(2, len(b)) + b


def _attestation_object_for(app_id: str) -> bytes:
    """A well-formed App Attest envelope (`fmt`, an empty `x5c`, `authData`)
    whose RP-ID hash names ``app_id`` — shaped right, signed by nobody."""
    auth_data = hashlib.sha256(app_id.encode()).digest() + bytes(5)
    return (
        _hdr(5, 3)
        + _text("fmt") + _text("apple-appattest")
        + _text("attStmt") + _hdr(5, 1) + _text("x5c") + _hdr(4, 0)
        + _text("authData") + _bstr(auth_data)
    )


# ── driving the wire ──────────────────────────────────────────────────────────


def _mint_nonce(nest) -> dict:
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.account.age_nonce", {})


def _register(nest, sk: SigningKey, handle: str, age_claim: dict | None) -> None:
    actor = bytes(sk.verify_key)
    ts = int(time.time() * 1000)
    body = {
        "actor_id": actor.hex(),
        "handle": handle,
        "timestamp": ts,
        "signature": sk.sign(
            register_signed_message(actor, handle, NEST_DOMAIN, ts)).signature.hex(),
    }
    if age_claim is not None:
        body["age_claim"] = age_claim
    with WsRpcAnonClient(nest["url"]) as anon:
        anon.call("fauna.account.register", body)


def _claim(platform: str, nonce_hex: str, attestation_object: bytes, band: str = "18+") -> dict:
    return {
        "band": band,
        "attestation": {
            "platform": platform,
            "nonce": nonce_hex,
            "key_id": secrets.token_hex(32) if platform == "ios" else "",
            "attestation_object": attestation_object,
        },
    }


def _refusal(nest, sk: SigningKey, handle: str, claim: dict) -> RpcCallError:
    with pytest.raises(RpcCallError) as ei:
        _register(nest, sk, handle, claim)
    return ei.value


def _handle(prefix: str) -> str:
    return f"{prefix}{secrets.token_hex(3)}"


def _assert_no_account(nest, sk: SigningKey, why: str) -> None:
    with pytest.raises(RpcCallError) as ei:
        handshake_via_ws(nest["url"], sk)
    assert ei.value.code == NOT_REGISTERED, (
        f"{why}: the key must have no account; sign-in answered "
        f"{ei.value.code} {ei.value.details!r}"
    )


def _invalid(err: RpcCallError, what: str) -> str:
    assert err.code == ATTESTATION_INVALID, (
        f"{what}: expected {ATTESTATION_INVALID}, got {err.code} {err.details!r}"
    )
    return str(err.details)


# ── join-a-nest 16 ────────────────────────────────────────────────────────────


@pytest.mark.feature("join-a-nest")
def test_a_nonce_the_nest_never_minted_refuses_the_whole_admission(age_nest):
    """Outcome 16: an attestation over a nonce the nest never issued is not
    downgraded to a declared band — the whole admission is refused and no
    account appears. The same sign-up with no attestation (a declared claim)
    goes through, so the refusal is the attestation's."""
    nest = age_nest
    assert "ios" in _mint_nonce(nest)["attestation_platforms"], (
        "this build must hold an iOS verifier for the rule to be exercisable"
    )
    sk = SigningKey.generate()
    handle = _handle("forged")
    err = _refusal(nest, sk, handle,
                   _claim("ios", secrets.token_hex(32), _attestation_object_for(PINNED_IOS_APP_ID)))
    _invalid(err, "a never-minted nonce")
    _assert_no_account(nest, sk, "a refused admission")

    # The discriminator: the same key and handle, declared only, is admitted.
    _register(nest, sk, handle, {"band": "18+"})
    assert mint_token_via_handshake(nest["url"], sk), "the declared sign-up admits the key"


@pytest.mark.feature("join-a-nest")
def test_a_replayed_nonce_is_refused_because_the_first_attempt_spent_it(age_nest):
    """Outcome 16: a nonce the nest minted works once — a bad attestation over
    it is refused and spends it, so sending the same nonce again is refused as
    spent (not re-checked), and a fresh nonce is untouched by the spent one."""
    nest = age_nest
    nonce = _mint_nonce(nest)["nonce"]
    obj = _attestation_object_for(PINNED_IOS_APP_ID)

    first_sk = SigningKey.generate()
    first = _invalid(_refusal(nest, first_sk, _handle("first"), _claim("ios", nonce, obj)),
                     "a bad attestation over a fresh nonce")
    assert "nonce" not in first, (
        f"the first use got past the nonce check and was refused on the attestation: {first!r}"
    )
    _assert_no_account(nest, first_sk, "the first refused attempt")

    again_sk = SigningKey.generate()
    again = _invalid(_refusal(nest, again_sk, _handle("again"), _claim("ios", nonce, obj)),
                     "the replayed nonce")
    assert "already-used" in again, f"a replay must be refused as a spent nonce: {again!r}"
    _assert_no_account(nest, again_sk, "the replayed attempt")


@pytest.mark.feature("join-a-nest")
def test_an_attestation_naming_a_bundle_the_nest_does_not_pin_is_refused(age_nest):
    """Outcome 16: an attestation from another app's bundle is refused for
    that reason — the nest reads only the RP-ID hash inside the attestation
    and finds no pinned App ID for it — while the same envelope naming a
    pinned bundle gets past the bundle check (and fails later, on the missing
    chain), so the refusal is the bundle's."""
    nest = age_nest

    wrong_sk = SigningKey.generate()
    wrong = _invalid(
        _refusal(nest, wrong_sk, _handle("wrong"),
                 _claim("ios", _mint_nonce(nest)["nonce"], _attestation_object_for(UNPINNED_IOS_APP_ID))),
        "an unpinned bundle",
    )
    assert "pinned app id" in wrong, f"refused for the wrong reason: {wrong!r}"
    _assert_no_account(nest, wrong_sk, "the unpinned-bundle attempt")

    pinned_sk = SigningKey.generate()
    pinned = _invalid(
        _refusal(nest, pinned_sk, _handle("pinned"),
                 _claim("ios", _mint_nonce(nest)["nonce"], _attestation_object_for(PINNED_IOS_APP_ID))),
        "a pinned bundle with no chain",
    )
    assert "pinned app id" not in pinned, (
        f"a pinned bundle must pass the bundle check and fail on the chain: {pinned!r}"
    )
    _assert_no_account(nest, pinned_sk, "the pinned-bundle attempt")


@pytest.mark.feature("join-a-nest")
def test_a_platform_the_nest_cannot_check_is_admitted_as_declared_not_verified(age_nest):
    """Outcome 16, the other side of the line: a platform this nest holds no
    verifier for is not a failed check. The admission goes through as a
    declared claim and the account holds no verified band row (a declared
    claim is never stored as a fact)."""
    nest = age_nest
    reply = _mint_nonce(nest)
    assert "android" not in reply["attestation_platforms"], (
        f"this build is expected to hold no android verifier: {reply['attestation_platforms']!r}"
    )
    sk = SigningKey.generate()
    handle = _handle("noverifier")
    _register(nest, sk, handle,
              _claim("android", reply["nonce"], b"not-a-real-verdict-token"))
    assert mint_token_via_handshake(nest["url"], sk), "the admission goes through"
    with WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key),
                          signing_key=bytes(sk)) as ws:
        status = ws.call("fauna.family.status", {})
    assert status.get("age_band") is None, (
        f"a declared-only claim must mint no verified band: {status.get('age_band')!r}"
    )
