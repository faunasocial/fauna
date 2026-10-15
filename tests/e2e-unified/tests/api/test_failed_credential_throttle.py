"""tier_3 E2E: the failed-credential throttle on the bearer WS upgrade and the
device handshake.

`bins/fauna-nest/src/failed_credential_throttle.rs`, owner
`transport-connection.md` § Abuse posture → *The failed-credential throttle*.
A client that keeps presenting a bearer the nest has refused is answered `429`
with a `Retry-After` once its `(source IP × claimed actor)` bucket has absorbed
`FAILED_CREDENTIAL_MAX` (10) refusals in a sliding minute — the answer the
native client's dial budget holds every dial to the nest on
(`transport-connection.md` § *A `429` on the upgrade holds every dial to that
nest*). Before it, a sync agent holding a dead credential was answered `401`
eight times a second for four days.

The unit tests in the module pin the bucket; this is the wiring, against the
real binary: that the upgrade handler actually spends the bucket on a refused
bearer, answers the real `429` + header past it, and — the half that makes it
safe — never refuses a VALID bearer, even one for the very actor the flood is
claiming, from the very same address.

Every bucket here keys on a claimed actor minted for this test, so it spends
nothing any other test on the session-scoped nest can reach. Not a timing test
(convention 14): the window is a count over a sliding minute, and a slow
machine spreading the burst can only make it more forgiving — the assertions
allow the trip anywhere up to a generous bound rather than at an exact attempt.
"""

import secrets

import pytest
import websocket

from common.auth import create_actor_and_register, ws_sslopt

pytestmark = [pytest.mark.tier_3]

# The shipped budget (`FAILED_CREDENTIAL_MAX`). A trip is expected on the
# attempt after it; the bound below leaves room for a window that drained a
# little while a slow box dialled.
BUDGET = 10
ATTEMPTS = 40

# The client's hold cap (`dial_budget::MAX_RETRY_AFTER`): a nest asking for
# longer would be asking for nothing.
CLIENT_HOLD_CAP_SECS = 15 * 60


def _ws_url(nest, actor_hex: str) -> str:
    base = nest["url"].replace("https://", "wss://").replace("http://", "ws://")
    return f"{base.rstrip('/')}/api/v1/ws/{actor_hex}"


def _dial(nest, actor_hex: str, token: str):
    """One upgrade. Returns `(status, headers)`; 101 for an accepted upgrade."""
    url = _ws_url(nest, actor_hex)
    try:
        sock = websocket.create_connection(
            url,
            subprotocols=["fauna.v1", f"bearer.{token}"],
            timeout=10,
            sslopt=ws_sslopt(url),
        )
    except websocket.WebSocketBadStatusException as e:
        return e.status_code, {k.lower(): v for k, v in (e.resp_headers or {}).items()}
    sock.close()
    return 101, {}


def _flood_until_throttled(nest, actor_hex: str) -> tuple[int, dict]:
    """Dial with a dead bearer until the nest answers anything but 401.

    Returns `(attempt number, headers)` of the first non-401 answer.
    """
    dead = secrets.token_hex(32)
    for n in range(1, ATTEMPTS + 1):
        status, headers = _dial(nest, actor_hex, dead)
        if status == 401:
            continue
        assert status == 429, (
            f"attempt {n} with a dead bearer answered {status}; the only "
            "answers a refused bearer may get are 401 and, past the budget, 429"
        )
        return n, headers
    pytest.fail(
        f"{ATTEMPTS} upgrades presenting a dead bearer for one claimed actor "
        "were all answered 401 — the nest has no failed-credential throttle, so "
        "a client stuck on a dead credential is never told to back off (the "
        "2026-09-24 flood: eight a second for four days)"
    )


def test_a_dead_bearer_flood_is_answered_429_with_a_retry_after(nest_instance):
    claimed = secrets.token_bytes(32).hex()
    n, headers = _flood_until_throttled(nest_instance, claimed)
    assert n > BUDGET, (
        f"the throttle answered 429 on attempt {n}, inside the {BUDGET}-refusal "
        "budget — a correct client refused once or twice must keep its own 401"
    )
    assert "retry-after" in headers, "the 429 must carry Retry-After"
    secs = int(headers["retry-after"])
    assert 0 < secs <= CLIENT_HOLD_CAP_SECS, (
        f"Retry-After {secs}s is outside (0, {CLIENT_HOLD_CAP_SECS}] — the client "
        "caps any hold at 15 min"
    )
    # And it stays throttled: the flood does not earn a fresh budget.
    for _ in range(5):
        status, _ = _dial(nest_instance, claimed, secrets.token_hex(32))
        assert status == 429


def test_a_valid_bearer_is_never_the_throttles_to_refuse(nest_instance):
    """The lockout half: the flood claims a REAL actor's id from the same
    address, and that actor's valid bearer still upgrades — the throttle counts
    refusals, so a credential the nest accepts never meets it. A neighbour on
    the same address claiming another actor keeps its own budget (its dead
    bearer is still answered 401, not 429)."""
    user = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    _flood_until_throttled(nest_instance, user["actor_id_hex"])

    status, _ = _dial(nest_instance, user["actor_id_hex"], user["token"])
    assert status == 101, (
        f"the actor's valid bearer was answered {status} while a flood claiming "
        "its id was throttled from the same address — the throttle must never "
        "refuse a credential the nest accepts"
    )

    neighbour = secrets.token_bytes(32).hex()
    status, _ = _dial(nest_instance, neighbour, secrets.token_hex(32))
    assert status == 401, (
        f"a neighbour claiming a different actor from the same address was "
        f"answered {status}; its bucket is its own and holds no refusals yet"
    )


# ── fauna.auth.device_handshake over the anonymous WS ────────────────────────


def _handshake_refusal_code(nest, actor: dict, device_key) -> str:
    """One renewal-grant mint; the error code the nest refused it with."""
    from clients._ws_rpc_core import RpcCallError
    from helpers.enrollment import device_sign_in

    with pytest.raises(RpcCallError) as refused:
        device_sign_in(nest["url"], actor["actor_id_bytes"], device_key)
    return refused.value.code


def test_an_unregistered_renewal_key_flood_is_answered_rate_limited(nest_instance):
    """The handshake arm of the throttle, keyed `(source × claimed renewal
    key)`: a key with no grant is refused `not_registered` until its bucket
    has absorbed the budget, then `fauna.protocol.rate_limited` — and a
    different claimed key from the same address keeps its own budget."""
    from nacl.signing import SigningKey

    actor = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    flooded = SigningKey(secrets.token_bytes(32))
    codes = [
        _handshake_refusal_code(nest_instance, actor, flooded) for _ in range(ATTEMPTS)
    ]
    allowed = {"fauna.auth.not_registered", "fauna.protocol.rate_limited"}
    assert set(codes) <= allowed, (
        f"an unregistered renewal key may only be refused {sorted(allowed)}; "
        f"got {sorted(set(codes) - allowed)}"
    )
    assert "fauna.protocol.rate_limited" in codes, (
        f"{ATTEMPTS} mints with an unregistered renewal key were all answered "
        "`not_registered` — the handshake has no failed-credential throttle"
    )
    first_limited = codes.index("fauna.protocol.rate_limited") + 1
    assert first_limited > BUDGET, (
        f"the throttle answered rate_limited on attempt {first_limited}, inside "
        f"the {BUDGET}-refusal budget"
    )
    # Once tripped it stays tripped: the flood does not earn a fresh budget.
    assert set(codes[first_limited - 1 :]) == {"fauna.protocol.rate_limited"}

    neighbour = SigningKey(secrets.token_bytes(32))
    assert (
        _handshake_refusal_code(nest_instance, actor, neighbour)
        == "fauna.auth.not_registered"
    ), "a different claimed key from the same address holds its own budget"
