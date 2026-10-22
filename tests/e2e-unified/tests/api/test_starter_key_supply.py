"""tier_3 API: the nest's promises about your supply of starter keys (MLS key
packages), witnessed at the wire with no app in the loop.

The ``[nest]`` outcomes of ``docs/features/encryption-settings.md`` that no
test asserted until now:

* **6** — when the one-use keys run out, a reusable last-resort key keeps you
  reachable, and it is never used up;
* **7** — a starter key expires after thirty days: never handed out after, and
  no longer counted;
* **12** — someone looking you up learns only whether you can be reached,
  never how many keys you have left;
* **13** — nobody takes your keys anonymously, and how fast any one nest can
  take them is limited;
* **14** — your nest refuses a key published under someone else's identity.

Goal docs: ``docs/goal/architecture/federation.md`` § Key packages — privacy &
exhaustion; ``docs/goal/behavior/direct-messages.md`` § Key Package Management;
``docs/goal/ui/conversations.md`` § Reactions & message delete (the
leaf-credential binding).

Every key is a REAL key package minted for its identity by the
``mls-keypackage-gen`` helper (``conv_api.mint_key_packages``): the upload
choke point parses and binds each one, so a fabricated blob would test the
parser, not the promise.

Latency-independent (convention 14): every assertion reads returned state. The
thirty days are reached through the ``test-hooks`` endpoint
``POST /api/v1/test/keypackages/age``, which moves ONE actor's keys back in time
and nothing else, after which the production reads run unchanged — never a wait.
"""

from __future__ import annotations

import json
import urllib.request

import pytest

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from clients.ws_rpc_federation_client import FederationChannelClient
from common.auth import create_actor_and_register
from tests.api.conv_api import mint_key_packages

pytestmark = pytest.mark.tier_3

UPLOAD = "fauna.conversations.keypackage.upload"
COUNT = "fauna.conversations.keypackage.count"
ADDRESSABLE = "fauna.conversations.keypackage.addressable"
FETCH = "fauna.conversations.keypackage.fetch"
FED_FETCH = "fauna.federation.keypackage.fetch"
BY_HANDLE = "fauna.actor.by_handle"

DAY = 24 * 3600
#: `keypackage_upload_handler`'s expiry — `direct-messages.md` § Key Package
#: Management.
EXPIRY_SECS = 30 * DAY
#: How close to the thirty-day line the expiry is pinned from each side.
EXPIRY_MARGIN_SECS = 3600


# ── harness ──────────────────────────────────────────────────────────────────


def _fresh_actor(nest) -> dict:
    """A registered account (with a handle) no other test shares."""
    return create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"]
    )


def _client(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"])
    )


def _mint(actor, n: int) -> list[bytes]:
    return mint_key_packages(bytes(actor["signing_key"]), n)


def _upload(nest, actor, packages: list[bytes], *, last_resort: bool = False) -> int:
    with _client(nest, actor) as owner:
        return owner.call(
            UPLOAD, {"packages": [bytes(p) for p in packages], "last_resort": last_resort}
        )["stored"]


def _count(nest, actor) -> int:
    with _client(nest, actor) as owner:
        return owner.call(COUNT, {"actor_id": actor["actor_id_hex"]})["count"]


def _take(nest, taker, target) -> bytes | None:
    """One same-nest fetch of ``target``'s key by ``taker`` — what starting a
    conversation does."""
    with _client(nest, taker) as client:
        key = client.call(FETCH, {"actor_id": target["actor_id_hex"]}).get("key_package")
    return bytes(key) if key is not None else None


def _addressable(nest, asker, target) -> bool:
    """The signed-in by-id reachability probe ``asker`` makes about ``target``."""
    with _client(nest, asker) as client:
        return client.call(ADDRESSABLE, {"actor_id": target["actor_id_hex"]})["addressable"]


def _look_up(nest, target) -> dict:
    """The anonymous lookup anyone — on any nest, signed in or not — makes."""
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call(BY_HANDLE, {"handle": target["handle"]})


def _age(nest, actor, secs: int) -> int:
    """Move ``actor``'s keys ``secs`` back in time; how many moved."""
    body = json.dumps({"actor_id": actor["actor_id_hex"], "secs": secs}).encode()
    req = urllib.request.Request(
        f"{nest['url']}/api/v1/test/keypackages/age",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"key-package aging hook returned {resp.status}"
        return json.loads(resp.read())["aged"]


# ── 6: the last-resort key ───────────────────────────────────────────────────


@pytest.mark.feature("encryption-settings")
def test_once_the_one_use_keys_run_out_the_last_resort_key_keeps_you_reachable(
    nest_instance,
):
    """encryption-settings 6 (`federation.md` § Key packages — privacy &
    exhaustion; `direct-messages.md:63`)."""
    nest = nest_instance
    owner, starter = _fresh_actor(nest), _fresh_actor(nest)
    [one_use] = _mint(owner, 1)
    [last_resort] = _mint(owner, 1)
    assert _upload(nest, owner, [one_use]) == 1
    assert _upload(nest, owner, [last_resort], last_resort=True) == 1
    assert _count(nest, owner) == 1, (
        "the supply counts the one-use keys only — the last-resort key is not "
        "part of what runs low"
    )

    assert _take(nest, starter, owner) == one_use, "the one-use key goes first"
    assert _count(nest, owner) == 0

    for attempt in ("first", "second", "third"):
        assert _take(nest, starter, owner) == last_resort, (
            f"with the one-use keys gone, the {attempt} conversation must still be "
            "handed the last-resort key"
        )
    assert _look_up(nest, owner)["addressable"] is True, (
        "the owner must still read as reachable: the last-resort key is never used up"
    )


# ── 7: thirty days ───────────────────────────────────────────────────────────


@pytest.mark.feature("encryption-settings")
def test_a_starter_key_expires_after_thirty_days(nest_instance):
    """encryption-settings 7 (`direct-messages.md` § Key Package Management):
    pinned from both sides of the line — a key an hour short of thirty days is
    still counted and reachable; an hour past it, no key is counted, handed out
    or advertised, the last-resort one included."""
    nest = nest_instance
    owner, starter = _fresh_actor(nest), _fresh_actor(nest)
    assert _upload(nest, owner, _mint(owner, 2)) == 2
    assert _upload(nest, owner, _mint(owner, 1), last_resort=True) == 1

    assert _age(nest, owner, EXPIRY_SECS - EXPIRY_MARGIN_SECS) == 3
    assert _count(nest, owner) == 2, "a key not yet thirty days old still counts"
    assert _look_up(nest, owner)["addressable"] is True

    _age(nest, owner, 2 * EXPIRY_MARGIN_SECS)
    assert _count(nest, owner) == 0, "a key past thirty days no longer counts"
    assert _take(nest, starter, owner) is None, (
        "an expired key — one-use or last-resort — must never be handed out"
    )
    assert _look_up(nest, owner)["addressable"] is False, (
        "with every key expired the owner must read as unreachable"
    )


# ── 12: a lookup learns reachable-or-not, nothing more ───────────────────────


@pytest.mark.feature("encryption-settings")
def test_a_lookup_learns_only_whether_you_can_be_reached(nest_instance):
    """encryption-settings 12 (`federation.md` § Key packages — privacy &
    exhaustion: a boolean on ``by_handle``, never a count).

    The lookup's whole reply is compared at three supplies — three keys, one,
    none: identical at three and one, and only the reachable bit moves at none.
    Anything in the reply that tracked the supply would break the first
    equality."""
    nest = nest_instance
    owner, starter = _fresh_actor(nest), _fresh_actor(nest)
    assert _upload(nest, owner, _mint(owner, 3)) == 3

    at_three = _look_up(nest, owner)
    assert at_three["addressable"] is True, at_three
    _take(nest, starter, owner)
    _take(nest, starter, owner)
    at_one = _look_up(nest, owner)
    assert at_one == at_three, (
        "a lookup must read the same at three keys as at one — it learns "
        f"reachable-or-not, never how many: {at_three} vs {at_one}"
    )

    _take(nest, starter, owner)
    at_none = _look_up(nest, owner)
    assert at_none["addressable"] is False, at_none
    assert {k: v for k, v in at_none.items() if k != "addressable"} == {
        k: v for k, v in at_three.items() if k != "addressable"
    }, f"only the reachable bit may move: {at_three} vs {at_none}"


@pytest.mark.feature("encryption-settings")
def test_a_signed_in_neighbour_learns_only_whether_you_can_be_reached(nest_instance):
    """encryption-settings 12 (`federation.md` § Key packages — privacy &
    exhaustion: the starter-key count is its owner's alone).

    A second account on the SAME nest is someone looking you up too: asking
    your count is refused, while your own count, the anonymous lookup and the
    by-id ``keypackage.addressable`` boolean still answer — the bit, never the
    number. The boolean also counts the last-resort key the count leaves out."""
    nest = nest_instance
    owner, neighbour = _fresh_actor(nest), _fresh_actor(nest)
    assert _upload(nest, owner, _mint(owner, 2)) == 2

    with _client(nest, neighbour) as client:
        with pytest.raises(RpcCallError) as refused:
            client.call(COUNT, {"actor_id": owner["actor_id_hex"]})
    assert refused.value.code == "fauna.conversations.permission_denied", refused.value
    assert _count(nest, owner) == 2, "the owner's own count still answers"

    assert _look_up(nest, owner)["addressable"] is True
    assert _addressable(nest, neighbour, owner) is True

    _take(nest, neighbour, owner)
    _take(nest, neighbour, owner)
    assert _addressable(nest, neighbour, owner) is False, (
        "with no key left the boolean must read unreachable"
    )
    assert _upload(nest, owner, _mint(owner, 1), last_resort=True) == 1
    assert _count(nest, owner) == 0, "the count leaves the last-resort key out"
    assert _addressable(nest, neighbour, owner) is True, (
        "an owner down to the last-resort key is still reachable — the boolean "
        "must count it, as the lookup does"
    )
    assert _look_up(nest, owner)["addressable"] is True


# ── 13: never anonymous, and throttled per nest ──────────────────────────────


#: Keys planted for the drain; well above the per-target budget so the
#: budget, not an empty supply, is what stops it.
DRAIN_SUPPLY = 100
#: At most this many one-time keys leave one user's pool through the cross-nest
#: fetch per hour, summed over every originating nest and source
#: (`bridge_rate_limit::KEYPACKAGE_FETCH_TARGET_LIMITER_CONFIG`, ruled
#: 2026-10-17); every fetch past it is served the last-resort key instead.
TARGET_BUDGET = 10


@pytest.mark.feature("encryption-settings")
def test_nobody_takes_starter_keys_anonymously_and_another_nest_cannot_drain_them(
    two_nodes,
):
    """encryption-settings 13 (`federation.md` § Key packages — privacy &
    exhaustion: the nest-signature gate — fetch is never anonymous — and the
    per-TARGET budget, the one bound a stranger's fresh keypairs cannot
    multiply: past it a fetch is served the reusable last-resort key and
    consumes nothing, so the target stays reachable and the pool stands).

    Its own two nests (``two_nodes``): the drain spends the owner's budget for
    an hour and may fill the home nest's per-source throttle for the kind,
    neither of which may reach a sibling test's fetch on the shared nest."""
    home, other = two_nodes["nest_a"], two_nodes["nest_b"]
    owner = _fresh_actor(home)
    assert _upload(home, owner, _mint(owner, DRAIN_SUPPLY)) == DRAIN_SUPPLY

    # Anonymous: neither the same-nest kind nor the cross-nest kind is
    # reachable from a connection that has proven no identity.
    with WsRpcAnonClient(home["url"]) as anon:
        for kind, payload in (
            (FETCH, {"actor_id": owner["actor_id_hex"]}),
            (FED_FETCH, {"target_actor_id": owner["actor_id_hex"]}),
        ):
            with pytest.raises(RpcCallError) as refused:
                anon.call(kind, payload)
            assert refused.value.code == "fauna.protocol.unauthenticated", (
                f"{kind} from an anonymous connection must be refused as "
                f"unauthenticated; got {refused.value.code}"
            )
    assert _count(home, owner) == DRAIN_SUPPLY, "the anonymous attempts took nothing"

    # The owner's reusable last-resort key — what every fetch past the
    # per-target budget is served. It is not part of the one-time count.
    last_resort = _mint(owner, 1)[0]
    assert _upload(home, owner, [last_resort], last_resort=True) == 1
    assert _count(home, owner) == DRAIN_SUPPLY, "the count is the one-time pool alone"

    # Another nest, over the signed federation channel, as fast as it can.
    # Every fetch is answered — the target stays reachable — but once the
    # owner's budget is spent the answer is the last-resort key, consuming
    # nothing. The channel's own per-source throttle may cut the run short
    # first; that too leaves the supply standing.
    taken, served_last_resort, throttled = 0, 0, None
    with FederationChannelClient(initiator=other, target=home) as fed:
        for _ in range(DRAIN_SUPPLY):
            try:
                reply = fed.call(FED_FETCH, {"target_actor_id": owner["actor_id_hex"]})
            except RpcCallError as refused:
                throttled = refused
                break
            key = reply.get("key_package")
            assert key is not None, "every fetch serves a key — the target stays reachable"
            if bytes(key) == bytes(last_resort):
                served_last_resort += 1
            else:
                taken += 1
    if throttled is not None:
        assert throttled.code == "fauna.protocol.rate_limited", throttled
    assert 0 < taken <= TARGET_BUDGET, (
        f"another nest took {taken} one-time keys: the per-target budget of "
        f"{TARGET_BUDGET} must stop a drain long before the supply is gone"
    )
    assert served_last_resort > 0 or throttled is not None, (
        f"another nest took {taken} keys in a row and was never slowed: past the "
        "budget every fetch is served the last-resort key"
    )
    left = _count(home, owner)
    assert left == DRAIN_SUPPLY - taken and left > 0, (
        f"the budget must leave the supply standing: {taken} taken, {left} left"
    )


# ── 14: a key under someone else's identity ──────────────────────────────────


@pytest.mark.feature("encryption-settings")
def test_the_nest_refuses_a_starter_key_published_under_someone_elses_identity(
    nest_instance,
):
    """encryption-settings 14 (`ui/conversations.md` § Reactions & message
    delete — the leaf-credential binding, enforced at the upload choke point).

    A signed-in account publishes a real key package minted for a DIFFERENT
    identity — the victim's — as its own, then as a last-resort key; both are
    refused, and nobody starting a conversation with either account is ever
    handed it."""
    nest = nest_instance
    forger, victim, starter = _fresh_actor(nest), _fresh_actor(nest), _fresh_actor(nest)
    forged = _mint(victim, 1)

    for last_resort in (False, True):
        with pytest.raises(RpcCallError) as refused:
            _upload(nest, forger, forged, last_resort=last_resort)
        assert refused.value.code == "fauna.conversations.invalid_params", refused.value

    assert _count(nest, forger) == 0, "nothing was stored under the forger"
    assert _take(nest, starter, forger) is None, "no forged key is ever handed out"
    assert _take(nest, starter, victim) is None, (
        "and none is handed out as the victim's either"
    )

    # The beside-control: the same forger's OWN key is accepted, so the refusal
    # above is the identity binding and not a broken upload.
    assert _upload(nest, forger, _mint(forger, 1)) == 1
