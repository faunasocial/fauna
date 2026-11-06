"""tier_3 API e2e: one handle, addressable on every domain the nest hosts.

`docs/features/your-address-on-every-domain.md`, the two ``[nest]`` outcomes,
observed from outside: a real SMTP peer at the nest's MX, the user's own mail
read over WS-RPC, and the outside server (the stub MX) a send reaches.

* **1** — mail to the handle at any hosted domain reaches its owner
  (`mail-multidomain.md` § The model: one handle, addressable on every active
  domain). The user holds no alias anywhere: the handle alone routes it.
* **4** — the handle sends as itself from any of the nest's domains, and the
  message is DKIM-signed for the domain it is sent from (§ Default:
  cross-domain submission allowed; § Signing-key selection at outbound time).
  Sent through the app's own door, ``fauna.email.send``; an independent
  verifier (dkimpy) checks each signature against that domain's published key.

The session's mail venue (``mail_bridge_mta``) hosts the primary domain and
``second.test``, each with its own signing key.
"""

from __future__ import annotations

import secrets
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from conftest import _provision_msek_recipient
from helpers.budgets import MAIL_OUTBOUND_CYCLE_S
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _find_tagged,
    _poke_outbound_bridge,
    dkim_signature_tags,
    verify_each_dkim_signature,
)
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

OUTSIDE = "external.test"


def _handle_user(nest_instance, seal_helper_binary):
    """A fresh user with a handle and mail credentials, and no alias on any
    domain — everything addressed to it below resolves through the handle."""
    actor = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    return _provision_msek_recipient(
        nest_instance=nest_instance, seal_helper_binary=seal_helper_binary, domain=None,
        local_part=actor["handle"], password=f"h-{secrets.token_hex(6)}", actor=actor,
    )


def _ws(nest_instance, user) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest_instance["url"], actor_id=user.actor_id,
        signing_key=bytes(user.recipient["signing_key"]),
    )


def _deliver(mta, rcpt: str, token: str, *, rcpt_code: str = "250") -> None:
    deadline = time.monotonic() + 40.0
    message = (
        f"From: <sender@{OUTSIDE}>\r\nTo: <{rcpt}>\r\nSubject: every domain {token}\r\n"
        f"Message-ID: <{token}@{OUTSIDE}>\r\nDate: Mon, 25 May 2026 12:00:00 +0000\r\n"
        f"\r\nBody of {token}.\r\n"
    ).encode()
    with _connect_smtp_starttls(mta.mx_port, mta.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{OUTSIDE}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", rcpt_code, deadline)
        if rcpt_code == "250":
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(message)
            conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("your-address-on-every-domain")
def test_mail_to_the_handle_at_any_hosted_domain_reaches_its_owner(
    nest_instance, mail_bridge_mta, seal_helper_binary
):
    mta = mail_bridge_mta
    user = _handle_user(nest_instance, seal_helper_binary)
    handle = user.username

    # The MX answers 250 to DATA only once the nest has filed the message, so
    # the inbox read after the sessions needs no wait.
    for domain in (mta.domain, mta.second_domain):
        _deliver(mta, f"{handle}@{domain}", f"to-{domain}")
    # Control: a domain this nest does not host is refused at RCPT.
    _deliver(mta, f"{handle}@unhosted.test", "to-unhosted", rcpt_code="550")

    with _ws(nest_instance, user) as ws:
        messages = ws.call("fauna.email.inbox.fetch", {"after_uid": 0, "limit": 50})["messages"]
    assert len(messages) == 2, (
        f"the handle's owner holds {len(messages)} messages, not one per hosted domain"
    )


@pytest.mark.feature("your-address-on-every-domain")
def test_the_handle_sends_from_any_domain_signed_for_that_domain(
    nest_instance, mail_bridge_mta, seal_helper_binary
):
    mta = mail_bridge_mta
    user = _handle_user(nest_instance, seal_helper_binary)
    handle = user.username
    records = {mta.domain: mta.dkim_records, mta.second_domain: mta.second_dkim_records}
    assert all(records.values()), "the venue published no signing keys to verify against"

    def _send_from(domain: str) -> bytes:
        token = f"from{secrets.token_hex(5)}"
        rcpt = f"{token}@{OUTSIDE}"
        raw = (
            f"From: <{handle}@{domain}>\r\nTo: <{rcpt}>\r\nSubject: every domain {token}\r\n"
            f"Message-ID: <{token}@{domain}>\r\nDate: Mon, 25 May 2026 12:00:00 +0000\r\n"
            "MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n"
            f"\r\nSent as {handle} from {domain}.\r\n"
        ).encode()
        with _ws(nest_instance, user) as ws:
            ws.call("fauna.email.send", {"recipients": [rcpt], "raw_rfc5322": raw})
        _poke_outbound_bridge(mta.nest_url)
        return wait_until(
            lambda: _find_tagged(mta.stub_mx, token), MAIL_OUTBOUND_CYCLE_S, interval=0.5,
            diagnose=lambda: f"the send from {domain} never reached the outside server "
            f"(bridge log: {mta.log_file})",
        )

    # The primary domain is the control: the second domain signs the same way,
    # under its own key, not the primary's.
    for domain in (mta.domain, mta.second_domain):
        received = _send_from(domain)
        assert dkim_signature_tags(received, "d") == [domain, domain], (
            f"a send from {domain} is signed for "
            f"{dkim_signature_tags(received, 'd')!r}"
        )
        verified, queried = verify_each_dkim_signature(received, domain, records[domain])
        assert verified == list(records[domain]), (
            f"the send from {domain} does not verify against {domain}'s published keys: "
            f"{verified!r} (queried {queried!r})"
        )
