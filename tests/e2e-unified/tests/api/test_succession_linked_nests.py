"""tier_3 API e2e: a recovery reaches every nest the identity is linked to.

`docs/goal/behavior/succession-linked-nests.md` § Every nest the identity is
linked to, observed from outside two nests that each hold an account for the
same identity — the nest the recovery ran at (A) and a linked one (B):

* **30** — a nest accepts a recovery only against the registration chain it
  holds itself: B, holding no chain, refuses the statement; the same statement
  relayed to B by A over the federation channel changes nothing, even once B
  holds the chain; submitted to B directly it lands.
* **31** — the nest that applied the recovery keeps each linked nest it has
  not reached as *owed*, shown to the successor alone, and the entry stands
  until a device gets the statement through there (the device is played by
  hand: the shared delivery runs in an app's full pass, which no harness hosts).

Every test starts its own two nests: a recovery changes both for good.
"""

from __future__ import annotations

import secrets

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_federation_client import FederationChannelClient
from common.auth import create_actor_and_register, register_user
from helpers.succession import (
    mint_statement,
    register_recovery_kit,
    registration_chain,
    submit_statement,
    succession_statements,
)
from tests.api import ws_api

pytestmark = pytest.mark.tier_3

SUCCESSION_PUSH = "fauna.federation.succession.push"


@pytest.fixture
def linked_nests(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest_a, cleanup_a = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "linked-a")
    try:
        nest_b, cleanup_b = _start_dedicated_nest(
            request, nest_mode, tmp_path_factory, "linked-b"
        )
        try:
            yield nest_a, nest_b
        finally:
            cleanup_b()
    finally:
        cleanup_a()


def _client(nest, actor_id: bytes, seed: bytes) -> WsRpcAdminClient:
    return WsRpcAdminClient(nest["url"], actor_id=actor_id, signing_key=seed)


def _signs_in(nest, actor_id: bytes, seed: bytes) -> bool:
    """Whether the key still opens an account session at ``nest``; a
    superseded key is refused at the handshake."""
    try:
        with _client(nest, actor_id, seed) as ws:
            ws.call("fauna.account.get", {})
        return True
    except RpcCallError as err:
        assert "superseded" in str(err), f"refused for another reason: {err!r}"
        return False


def _identity_on_both(nest_a, nest_b) -> tuple[dict, str]:
    """One identity with an account on both nests and a recovery kit at A.
    Returns the actor and the kit's secret."""
    actor = create_actor_and_register(
        nest_a["port"], base_url=nest_a["url"], admin_signing_key=nest_a["admin"]["signing_key"]
    )
    register_user(
        nest_b["port"], actor["actor_id_hex"], base_url=nest_b["url"],
        admin_signing_key=nest_b["admin"]["signing_key"],
    )
    kit = register_recovery_kit(
        nest_a["url"], actor_id_hex=actor["actor_id_hex"],
        identity_seed_hex=bytes(actor["signing_key"]).hex(),
    )
    return actor, kit


def _replay_chain(src, dst, actor: dict) -> None:
    """Carry the chain ``src`` holds to ``dst`` over the account's own session,
    the way a linking client does (§ *The chain follows the link*)."""
    with _client(dst, actor["actor_id_bytes"], bytes(actor["signing_key"])) as ws:
        for record in registration_chain(src["url"], actor["actor_id_hex"]):
            ws.call("fauna.recovery.registration.submit", {"registration": record})


def _recover_at(nest, actor: dict, kit: str) -> tuple[str, bytes, bytes]:
    """Run the recovery at ``nest``. Returns the successor id hex, its seed
    and the statement."""
    seed = secrets.token_bytes(32)
    successor, statement = mint_statement(
        old_actor_id_hex=actor["actor_id_hex"], recovery_secret_hex=kit,
        successor_seed_hex=seed.hex(),
        registrations=registration_chain(nest["url"], actor["actor_id_hex"]),
    )
    assert submit_statement(nest["url"], statement) == successor
    return successor, seed, statement


@pytest.mark.feature("take-your-account-back")
def test_a_nest_accepts_a_recovery_only_against_its_own_records(linked_nests):
    nest_a, nest_b = linked_nests
    actor, kit = _identity_on_both(nest_a, nest_b)
    old_id, old_seed = actor["actor_id_bytes"], bytes(actor["signing_key"])

    _, _, statement = _recover_at(nest_a, actor, kit)
    assert not _signs_in(nest_a, old_id, old_seed), "control: A retired the old key"

    # B holds the account and no chain: it can verify nothing, so it refuses.
    with pytest.raises(RpcCallError) as refused:
        submit_statement(nest_b["url"], statement)
    assert refused.value.code.endswith("not_registered"), refused.value.code
    assert _signs_in(nest_b, old_id, old_seed)

    # With the chain carried to B, the statement relayed by A over the
    # federation channel still changes nothing: only B's own door applies it.
    _replay_chain(nest_a, nest_b, actor)
    with FederationChannelClient(initiator=nest_a, target=nest_b) as fed:
        pushed = fed.call(SUCCESSION_PUSH, {"statement": statement})
    assert pushed.get("recorded") is False, pushed
    assert _signs_in(nest_b, old_id, old_seed), "a relayed recovery retired the key at B"
    assert succession_statements(nest_b["url"], actor["actor_id_hex"]) == []

    # Control: the same statement at B's own door, against B's own chain, lands.
    submit_statement(nest_b["url"], statement)
    assert not _signs_in(nest_b, old_id, old_seed), "B's own door did not apply it"


@pytest.mark.feature("take-your-account-back")
def test_a_linked_nest_the_recovery_has_not_reached_stays_owed_until_a_device_gets_through(
    linked_nests,
):
    nest_a, nest_b = linked_nests
    actor, kit = _identity_on_both(nest_a, nest_b)
    old_id, old_seed = actor["actor_id_bytes"], bytes(actor["signing_key"])
    b_id = bytes.fromhex(nest_b["nest_id"])
    ws_api.add_pairing(nest_a["port"], actor, b_id, nest_url=nest_b["peer_url"])
    with _client(nest_a, old_id, old_seed) as ws:
        pairings = ws.call("fauna.pair.list", {})["pairings"]
    assert [bytes(p["private_nest_id"]) for p in pairings] == [b_id], (
        f"control: A holds the link to B before the recovery: {pairings!r}"
    )
    _replay_chain(nest_a, nest_b, actor)

    successor, seed, statement = _recover_at(nest_a, actor, kit)
    successor_id = bytes.fromhex(successor)
    assert bytes(SigningKey(seed).verify_key) == successor_id

    def owed(actor_id: bytes, key: bytes) -> list[tuple[bytes, bytes]]:
        with _client(nest_a, actor_id, key) as ws:
            status = ws.call("fauna.recovery.succession.status", {})
        # An empty list is absent from the wire.
        return [
            (bytes(o["old_actor_id"]), bytes(o["nest_id"]))
            for o in status.get("owed_nests", [])
        ]

    # A keeps B owed, and keeps it on every read, while B is not reached.
    for _ in range(2):
        assert owed(successor_id, seed) == [(old_id, b_id)], "B is not owed"
    assert _signs_in(nest_b, old_id, old_seed), "control: nothing has reached B yet"

    # Shown to the successor alone; and nobody else can clear it.
    bystander = create_actor_and_register(
        nest_a["port"], base_url=nest_a["url"], admin_signing_key=nest_a["admin"]["signing_key"]
    )
    bystander_key = (bystander["actor_id_bytes"], bytes(bystander["signing_key"]))
    assert owed(*bystander_key) == []
    with _client(nest_a, *bystander_key) as ws:
        with pytest.raises(RpcCallError) as denied:
            ws.call(
                "fauna.recovery.succession.owed_settle",
                {"old_actor_id": old_id, "nest_id": b_id},
            )
    assert denied.value.code.endswith("permission_denied"), denied.value.code
    assert owed(successor_id, seed) == [(old_id, b_id)]

    # A device gets through to B: the statement lands there, and the
    # successor settles the entry.
    assert submit_statement(nest_b["url"], statement) == successor
    assert not _signs_in(nest_b, old_id, old_seed)
    with _client(nest_a, successor_id, seed) as ws:
        ws.call(
            "fauna.recovery.succession.owed_settle", {"old_actor_id": old_id, "nest_id": b_id}
        )
    assert owed(successor_id, seed) == []
