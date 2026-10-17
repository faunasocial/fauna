"""tier_3: what a factory reset does to a nest that serves mail
(``docs/goal/architecture/nest/common.md`` § Factory reset).

Two outcomes of the ``factory-reset`` page, each on a dedicated mail venue of
its own, since a reset wipes every other test's state with it:

- **6** — a reset erases every account, handle, admin grant, domain, address
  and message on the nest;
- **14** — after a reset and a fresh claim with mail switched on, the nest
  accepts and delivers mail again.

The harness plays the supervisor twice over. ``common.nest.factory_reset_and_
restart`` plays it for the nest (stage, exit, respawn into the boot-time wipe).
The venue's host-spawned bridges need the other half: in production the
reset handler downs both s6 bridge services before it exits, and switching
mail on again brings them up into a fresh cold boot, which is the only point a
bridge enrols (``docs/goal/behavior/mail-bridge-lifecycle.md`` § Factory
reset). A bridge left running would loop on ``not_registered`` against the
wiped nest and never re-enrol, so the tests stop both bridges before the reset
and cold-start them only after the re-claim and mail-on.

``standalone_only``: both halves of that supervision act on host processes the
harness spawned. In the image s6 does both, so a docker witness is a different
act — the twin of ``factory_reset_and_restart`` in
``tests/platform/docker/helpers.py`` — not this test run in another mode.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.nest import factory_reset_and_restart
from helpers.mail_aliases import add_exact_alias
from helpers.mail_wire import _open_mail_gate_and_wait_for_the_mta
from helpers.recipient_seal_key import provision_recipient_seal_key
from tests.test_mail_bridge_mta import _deliver, _messages_held, _venue_member

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]


@pytest.fixture
def reset_mail_venue(request, nest_mode, tmp_path_factory):
    """A claimed mail nest with its MTA serving, of this test's own: the admin
    holds a recipient seal key the harness registered, so role mail to
    ``postmaster@`` lands in the admin's mailbox."""
    from conftest import _start_mail_venue

    handle, cleanup = _start_mail_venue(
        request, nest_mode, tmp_path_factory, "reset-mail-venue"
    )
    try:
        _admin_seal_key(handle.nest)
        _open_mail_gate_and_wait_for_the_mta(handle)
        yield handle
    finally:
        cleanup()


def _ws(nest, who: dict) -> WsRpcAdminClient:
    sk = who["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _admin_call(nest, kind: str, payload: dict) -> dict:
    with _ws(nest, nest["admin"]) as ws:
        return ws.call(kind, payload)


def _admin_seal_key(nest) -> None:
    with _ws(nest, nest["admin"]) as ws:
        provision_recipient_seal_key(ws, nest["admin"]["actor_id_bytes"])


def _run_due(nest) -> None:
    """Execute every scheduled pending action now (the `test-hooks` route), so
    an admin grant's delay is not a wall-clock wait (convention 14)."""
    import urllib.request

    req = urllib.request.Request(
        f"{nest['url']}/api/v1/test/pending_actions/run_due", data=b"", method="POST"
    )
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"run_due hook returned {resp.status}"


def _users(nest) -> set[bytes]:
    with _ws(nest, nest["admin"]) as ws:
        return {bytes(u["actor_id"]) for u in ws.users_list_all()}


def _admins(nest) -> set[bytes]:
    return {bytes(a["actor_id"]) for a in _admin_call(nest, "fauna.admin.admins.list", {})["admins"]}


def _domains(nest) -> set[str]:
    """Every domain the nest still knows, active or soft-deleted."""
    listed = _admin_call(nest, "fauna.bridges.list_local_domains", {})
    return {d["domain_name"] for d in listed["active"] + listed["soft_deleted_within_30d"]}


def _forwarders(nest) -> list[dict]:
    return _admin_call(nest, "fauna.bridges.list_forwarders", {})["forwarders"]


def _own_aliases(nest, who: dict) -> list[dict]:
    with _ws(nest, who) as ws:
        return ws.call("fauna.bridges.list_account_aliases", {})["aliases"]


def _by_handle(nest, handle: str) -> dict:
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.actor.by_handle", {"handle": handle})


def _stop_bridges(handle) -> None:
    """Down both bridges, as the reset handler downs their s6 services before
    it exits. Only this venue's own processes are signalled."""
    for bridge in (handle.mta, handle.mda):
        if bridge.proc.poll() is None:
            bridge.proc.terminate()
            bridge.proc.wait(timeout=30)


def _reset(handle) -> None:
    """Stop the bridges, factory-reset the nest and re-claim it with the same
    admin identity."""
    _stop_bridges(handle)
    factory_reset_and_restart(handle.nest)


def _switch_mail_back_on(handle) -> None:
    """What the admin does after the re-claim, then what s6 does on the enable.

    The admin registers the domain again, puts the venue's spam posture back
    (both rested in the wiped database) and switches mail on. The harness then
    stands in for the two things the deployment does on its own: it hands the
    MTA its certificate again (the sealed blob rested in the wiped database;
    sealed to the bridge's preserved keypair, so the same bytes still open) and
    cold-starts both bridges. Each one enrols afresh on its preserved keypair
    and is auto-approved because mail is already on: the MTA only binds its
    SMTP listener once it is approved, so `respawn`'s wait on that listener
    is the proof of the re-enrolment.
    """
    from conftest import MTA_FIXTURE_SPAM_POSTURE

    nest = handle.nest
    with _ws(nest, nest["admin"]) as admin:
        admin.call(
            "fauna.bridges.add_local_domain",
            {"domain": handle.domain, "mta_sts_cert_mode": "per_host"},
        )
        admin.call("fauna.bridges.put_spam_policy", dict(MTA_FIXTURE_SPAM_POSTURE))
        admin.call("fauna.bridges.set_mail_enabled", {"enabled": True})
        admin.provision_tls_cert_blob(handle.mta.tls_blob)
    handle.mta.respawn(wait_for_serving=True)
    handle.mda.respawn(wait_for_serving=True)


@pytest.mark.feature("factory-reset")
def test_a_reset_erases_every_account_handle_grant_domain_address_and_message(
    reset_mail_venue,
):
    """factory-reset 6: seed each kind of record over the api, then reset and
    re-claim; every one of them is gone.

    Seeded: a member with a handle, granted admin; a second domain; an address
    of the admin's own and one of the member's; a forwarder; a message
    delivered to the admin (``postmaster@``) and one to the member's address.
    Observed after the reset and the re-claim with the same admin identity:
    no account but the admin's and the nest's own boot-time service rows, the
    admin alone on the roster, no domain, no
    forwarder, none of the admin's addresses, the admin's mailbox empty, the
    member's handle unresolvable and the member's handshake refused."""
    handle = reset_mail_venue
    nest = handle.nest
    admin = nest["admin"]
    # The rows the nest writes for itself at every boot (its co-located
    # content-processor service enrols on its kept keypair), as opposed to the
    # admin, the two mail bridges this venue enrolled, and what the test seeds.
    nest_own = _users(nest) - {
        admin["actor_id_bytes"], handle.mta.ed25519_pubkey, handle.mda.ed25519_pubkey,
    }

    member = _venue_member(handle)
    member_addr = add_exact_alias(nest["url"], member["signing_key"], handle.domain,
                                  f"m{secrets.token_hex(3)}")
    admin_addr = add_exact_alias(nest["url"], admin["signing_key"], handle.domain,
                                 f"a{secrets.token_hex(3)}")
    second_domain = f"reset-{secrets.token_hex(4)}.test"
    with _ws(nest, admin) as ws:
        ws.call("fauna.bridges.add_local_domain",
                {"domain": second_domain, "mta_sts_cert_mode": "per_host"})
        ws.call("fauna.bridges.create_forwarder", {
            "local_domain": handle.domain,
            "pattern": f"f{secrets.token_hex(3)}",
            "forward_target": "elsewhere@external.test",
        })
        grant = ws.call("fauna.admin.admins.add", {"actor_id": member["actor_id_bytes"]})
    assert grant["status"] == "pending", grant
    _run_due(nest)
    _deliver(handle, f"postmaster@{handle.domain}", "before the reset, to the admin")
    _deliver(handle, member_addr, "before the reset, to the member")

    before = _admin_call(nest, "fauna.admin.stats", {})
    assert before["total_users"] >= 2, f"precondition: the member is a user: {before!r}"
    assert _admins(nest) == {admin["actor_id_bytes"], member["actor_id_bytes"]}, (
        "precondition: the member was granted admin"
    )
    assert {handle.domain, second_domain} <= _domains(nest), "precondition: two domains"
    assert len(_forwarders(nest)) == 1, "precondition: one forwarder"
    assert admin_addr in {f"{a['pattern']}@{a['local_domain']}" for a in _own_aliases(nest, admin)}, (
        "precondition: the admin holds an address of their own"
    )
    assert _messages_held(nest, admin) >= 1 and _messages_held(nest, member) >= 1, (
        "precondition: both mailboxes hold the delivered mail"
    )
    assert _by_handle(nest, member["handle"])["actor_id"] == member["actor_id_hex"], (
        "precondition: the member's handle resolves"
    )

    _reset(handle)

    users = _users(nest)
    assert users == {admin["actor_id_bytes"]} | nest_own, (
        "after the reset the only accounts may be the re-claiming admin and the "
        "nest's own boot-time service rows — the member and both mail bridges' "
        f"rows must be gone: {sorted(u.hex()[:8] for u in users)}"
    )
    after = _admin_call(nest, "fauna.admin.stats", {})
    assert after["total_users"] == len(users), after
    assert _admins(nest) == {admin["actor_id_bytes"]}, "the member's admin grant must be gone"
    assert _domains(nest) == set(), "every mail domain must be gone"
    assert _forwarders(nest) == [], "every forwarder must be gone"
    assert _own_aliases(nest, admin) == [], "the admin's own addresses must be gone"
    assert _messages_held(nest, admin) == 0, "the admin's mailbox must be empty"
    with pytest.raises(RpcCallError) as unknown:
        _by_handle(nest, member["handle"])
    assert unknown.value.code == "fauna.actor.not_found", (
        f"the member's handle must be gone; got {unknown.value.code}"
    )
    with pytest.raises(RpcCallError) as refused:
        with _ws(nest, member) as ws:
            ws.call("fauna.bridges.list_account_aliases", {})
    assert refused.value.code == "fauna.auth.not_registered", (
        f"the member's account must be gone, so the handshake is refused; got {refused.value.code}"
    )


@pytest.mark.feature("factory-reset")
def test_mail_is_accepted_and_delivered_again_after_a_reset_and_reclaim(reset_mail_venue):
    """factory-reset 14: the venue delivers to the admin's mailbox; after a
    reset, a re-claim with the same identity and mail switched on, the bridges
    re-enrol on their kept keypairs and a fresh delivery to ``postmaster@``
    is accepted (250 on end-of-DATA) and lands in the admin's mailbox."""
    handle = reset_mail_venue
    nest = handle.nest
    postmaster = f"postmaster@{handle.domain}"

    _deliver(handle, postmaster, "before the reset")
    assert _messages_held(nest, nest["admin"]) == 1, "precondition: the venue delivers"

    _reset(handle)
    _switch_mail_back_on(handle)
    _admin_seal_key(nest)
    assert _messages_held(nest, nest["admin"]) == 0, "precondition: the reset emptied the mailbox"

    _deliver(handle, postmaster, "after the reset and the re-claim")
    assert _messages_held(nest, nest["admin"]) == 1, (
        "the message accepted after the re-claim must be delivered to the admin's mailbox"
    )
