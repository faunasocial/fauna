"""tier_3 API e2e: a mail sent from the app goes out only as its sender.

`docs/features/email-in-conversations.md` outcome 21, observed from outside
over the app's own door, `fauna.email.send` (`docs/goal/behavior/
mail-app-surface.md` § First-party client send — sender ownership at the app
door, both arms; the predicate is `mail-multidomain.md` § From: header
ownership): a `From:` on a domain the nest signs for must be the caller's handle
or an alias the resolver attributes to them — another member's alias or handle
is refused `fauna.email.permission_denied` — and a `From:` field naming other
than exactly one mailbox, or a message with other than one `From:` field, is
refused `fauna.email.invalid_params` before anything is sent.

The outside-observed twin of the in-process pins in
`bins/fauna-nest/tests/conformance_email_send_in_domain.rs` (the owned-alias,
another-actor's-alias, one-mailbox and one-From-field tests): here the caller is
a real account over the real socket, and an admitted send is seen leaving at the
outside server.
"""

from __future__ import annotations

import secrets

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.budgets import MAIL_OUTBOUND_CYCLE_S
from helpers.mail_wire import _find_tagged, _poke_outbound_bridge
from helpers.recipient_seal_key import provision_recipient_seal_key
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

OUTSIDE = "external.test"


def _admin_ws(nest) -> WsRpcAdminClient:
    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _ws(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"]),
    )


def _member(nest) -> dict:
    """A fresh account with a handle and its mail turned on (seal key on file —
    the sender's Sent copy is sealed to it)."""
    actor = create_actor_and_register(nest["port"], admin_signing_key=nest["admin"]["signing_key"])
    with _admin_ws(nest) as admin:
        provision_recipient_seal_key(admin, actor["actor_id_bytes"])
    return actor


def _message(from_field: str, token: str, *, extra_from: str | None = None) -> bytes:
    froms = f"From: {from_field}\r\n" + (f"From: {extra_from}\r\n" if extra_from else "")
    return (
        f"{froms}To: <{token}@{OUTSIDE}>\r\nSubject: from ownership {token}\r\n"
        f"Message-ID: <{token}@origin.invalid>\r\nDate: Mon, 25 May 2026 12:00:00 +0000\r\n"
        "MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n"
        f"\r\nfrom ownership {token}\r\n"
    ).encode()


def _send(nest, sender, raw: bytes, token: str):
    with _ws(nest, sender) as ws:
        return ws.call("fauna.email.send",
                       {"recipients": [f"{token}@{OUTSIDE}"], "raw_rfc5322": raw})


@pytest.mark.feature("email-in-conversations")
def test_a_mail_sent_from_the_app_goes_out_only_as_its_sender(nest_instance, mail_bridge_mta):
    nest, mta = nest_instance, mail_bridge_mta
    domain = mta.domain
    tag = secrets.token_hex(3)
    sender = _member(nest)
    other = _member(nest)
    with _ws(nest, sender) as ws:
        ws.call("fauna.bridges.create_account_alias", {
            "kind": "exact", "local_domain": domain, "pattern": f"mine{tag}",
            "controls": {"label": ""},
        })
    with _ws(nest, other) as ws:
        ws.call("fauna.bridges.create_account_alias", {
            "kind": "exact", "local_domain": domain, "pattern": f"theirs{tag}",
            "controls": {"label": ""},
        })

    # Admitted: the sender's handle and the sender's own alias, each seen
    # leaving at the outside server under that From.
    for own in (f"{sender['handle']}@{domain}", f"mine{tag}@{domain}"):
        token = f"own{secrets.token_hex(5)}"
        reply = _send(nest, sender, _message(f"<{own}>", token), token)
        assert reply["remote_queued"] == 1, f"a send as {own} must be queued: {reply!r}"
        _poke_outbound_bridge(mta.nest_url)
        received = wait_until(
            lambda: _find_tagged(mta.stub_mx, token), MAIL_OUTBOUND_CYCLE_S, interval=0.5,
            diagnose=lambda: f"the send as {own} never reached the outside server "
            f"(bridge log: {mta.log_file})",
        )
        assert own.encode() in received, f"the relayed message must be from {own}"

    # Refused as impersonation: another member's alias, and another member's handle.
    for foreign in (f"theirs{tag}@{domain}", f"{other['handle']}@{domain}"):
        token = f"foreign{secrets.token_hex(5)}"
        with pytest.raises(RpcCallError) as refused:
            _send(nest, sender, _message(f"<{foreign}>", token), token)
        assert refused.value.code == "fauna.email.permission_denied", (
            f"a send as {foreign} must be refused as not the sender's; got {refused.value!r}")
        assert "or an alias you own" in str(refused.value), (
            f"the refusal names what the address could have been: {refused.value!r}")
        assert _find_tagged(mta.stub_mx, token) is None

    # Refused before anything is sent: a From naming two mailboxes, and a
    # message carrying two From fields (the sender's own address in each).
    own = f"{sender['handle']}@{domain}"
    for raw_of, rule in (
        (lambda t: _message(f"{own}, ceo@bank.example", t), "exactly one mailbox"),
        (lambda t: _message(f"<{own}>", t, extra_from="<ceo@bank.example>"), "exactly one From"),
    ):
        token = f"shape{secrets.token_hex(5)}"
        with pytest.raises(RpcCallError) as refused:
            _send(nest, sender, raw_of(token), token)
        assert refused.value.code == "fauna.email.invalid_params", (
            f"{rule}: a request-validation refusal; got {refused.value!r}")
        assert rule in str(refused.value), f"the refusal names the rule: {refused.value!r}"
