"""Tier 3: what an SMTP peer sees when it mails a user's aliases — the delivery
and refusal outcomes of `docs/goal/behavior/mail-aliases.md`, witnessed from
outside over the real MTA bridge and the real WS-RPC wire.

The alias CRUD surface is covered over WS-RPC in `api/test_mail_aliases_user.py`
and the resolver order by the Rust handler tests; neither proves what a sending
MX actually gets at `RCPT TO`. Each test here provisions a fresh recipient on the
MTA fixture's domain, shapes its aliases through the user-class
`fauna.bridges.*_account_alias` kinds (what the aliases page calls), then speaks
SMTP: a delivered address answers `250` at RCPT and DATA and leaves a placed
message for the owner (`bridge_imap_messages` — the row IMAP reads); a refused
one answers the `550` reply the goal doc names, at RCPT.

Latency-independent (convention 14): the MTA commits ingest before it ACKs the
DATA dot and refuses at RCPT synchronously, so every assertion reads settled
state — no waits.
"""

import secrets
import sqlite3
import time

import pytest
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.mail_wire import _connect_smtp_starttls
from helpers.mail_aliases import add_exact_alias
from helpers.recipient_seal_key import provision_recipient_seal_key

pytestmark = pytest.mark.tier_3


def _admin_ws(nest_instance):
    sk = nest_instance["admin"]["signing_key"]
    return WsRpcAdminClient(nest_instance["url"], actor_id=bytes(sk.verify_key),
                            signing_key=bytes(sk))


def _user_ws(nest_instance, user):
    return WsRpcAdminClient(nest_instance["url"], actor_id=user["actor_id_bytes"],
                            signing_key=bytes(user["signing_key"]))


def _fresh_mailbox(nest_instance, domain):
    """A fresh user with an MLS pubkey on file (the MTA seals to it) and an
    exact canonical address on `domain`. Returns `(user, actor_id, handle)`."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    actor_id = user["actor_id_bytes"]
    handle = f"al{secrets.token_hex(4)}"
    with _admin_ws(nest_instance) as admin:
        add_exact_alias(nest_instance["url"], user["signing_key"], domain, handle)
        provision_recipient_seal_key(admin, actor_id)
    return user, actor_id, handle


def _create_alias(client, domain, pattern, kind="exact"):
    return client.call("fauna.bridges.create_account_alias", {
        "kind": kind, "local_domain": domain, "pattern": pattern,
        "controls": {"label": ""},
    })["alias_id"]


def _placed(db_path, actor_id):
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        return conn.execute(
            "SELECT COUNT(*) FROM bridge_imap_messages WHERE actor_id = ?", (actor_id,),
        ).fetchone()[0]
    finally:
        conn.close()


def _send(mta, rcpt, expect_rcpt="250", expect_reply=None):
    """One inbound transaction to `rcpt`. When RCPT is accepted the message is
    sent and DATA must 250; when refused, the transaction ends at RCPT and
    `expect_reply` (a substring) must appear in the refusal."""
    deadline = time.monotonic() + 30.0
    body = "\r\n".join([
        "From: Sender <peer@origin.test>",
        f"To: {rcpt}",
        f"Subject: alias delivery {secrets.token_hex(3)}",
        f"Message-ID: <{secrets.token_hex(8)}@origin.test>",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "alias delivery body",
    ]) + "\r\n"
    with _connect_smtp_starttls(mta.mx_port, mta.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<peer@origin.test>", "250", deadline)
        reply = conn.cmd(f"RCPT TO:<{rcpt}>", expect_rcpt, deadline)
        if expect_rcpt == "250":
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(body.encode())
            conn.cmd(".", "250", deadline)
        elif expect_reply is not None:
            text = reply if isinstance(reply, str) else repr(reply)
            assert expect_reply in text, (
                f"RCPT {rcpt} refusal must say {expect_reply!r}; got {text!r}")
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("mail-aliases")
def test_plus_suffix_reaches_the_owner_with_nothing_set_up(mail_bridge_mta, nest_instance):
    """Mail to `<address>+<anything>@` reaches the address's owner with no alias
    configured for the suffix (§ Kind 2 — +suffix sub-addressing, RFC 5233)."""
    mta = mail_bridge_mta
    _, actor_id, handle = _fresh_mailbox(nest_instance, mta.domain)
    before = _placed(nest_instance["db_path"], actor_id)
    _send(mta, f"{handle}+newsletters@{mta.domain}")
    assert _placed(nest_instance["db_path"], actor_id) == before + 1, (
        "a +suffix address must deliver to the base address's owner")


@pytest.mark.feature("mail-aliases")
def test_wildcard_prefix_address_reaches_the_owner(mail_bridge_mta, nest_instance):
    """Any address under a user's wildcard prefix reaches them (§ Kind 3 —
    Wildcard prefix): the prefix is the only thing configured."""
    mta = mail_bridge_mta
    user, actor_id, _ = _fresh_mailbox(nest_instance, mta.domain)
    prefix = f"wc{secrets.token_hex(3)}-"
    with _user_ws(nest_instance, user) as ws:
        _create_alias(ws, mta.domain, prefix, kind="wildcard_prefix")
    before = _placed(nest_instance["db_path"], actor_id)
    _send(mta, f"{prefix}some-shop@{mta.domain}")
    assert _placed(nest_instance["db_path"], actor_id) == before + 1, (
        "an address under the wildcard prefix must deliver to its owner")


@pytest.mark.feature("mail-aliases")
def test_disposable_refused_once_uses_or_lifetime_run_out(mail_bridge_mta, nest_instance):
    """A throwaway address takes mail while alive and answers `550 Address
    expired` once its uses are spent — and, separately, once its lifetime is past
    (§ Kind 5 — Disposable: whichever bound comes first)."""
    mta = mail_bridge_mta
    db_path = nest_instance["db_path"]
    user, actor_id, _ = _fresh_mailbox(nest_instance, mta.domain)
    with _user_ws(nest_instance, user) as ws:
        by_uses = ws.call("fauna.bridges.generate_disposable_alias", {"uses": 1})
        by_ttl = ws.call("fauna.bridges.generate_disposable_alias", {"ttl_days": 7})

    before = _placed(db_path, actor_id)
    _send(mta, by_uses["full_address"])
    assert _placed(db_path, actor_id) == before + 1, "a live disposable delivers"
    _send(mta, by_uses["full_address"], expect_rcpt="550", expect_reply="Address expired")

    # Lifetime: move the TTL alias's outer bound into the past (the clock the
    # resolver compares against is the only thing changed).
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute("UPDATE account_aliases SET expires_at = 1 WHERE alias_id = ?",
                     (by_ttl["alias_id"],))
        conn.commit()
    finally:
        conn.close()
    _send(mta, by_ttl["full_address"], expect_rcpt="550", expect_reply="Address expired")
    assert _placed(db_path, actor_id) == before + 1, "an expired disposable places nothing"


@pytest.mark.feature("mail-aliases")
def test_disabled_alias_refused_and_kept(mail_bridge_mta, nest_instance):
    """Mail to an address its owner switched off is refused `550 Address
    disabled`, while the alias row stays listed for the owner (§ Disable)."""
    mta = mail_bridge_mta
    user, actor_id, _ = _fresh_mailbox(nest_instance, mta.domain)
    pattern = f"off{secrets.token_hex(3)}"
    with _user_ws(nest_instance, user) as ws:
        alias_id = _create_alias(ws, mta.domain, pattern)
        ws.call("fauna.bridges.revoke_account_alias", {"alias_id": alias_id})
        before = _placed(nest_instance["db_path"], actor_id)
        _send(mta, f"{pattern}@{mta.domain}", expect_rcpt="550",
              expect_reply="Address disabled")
        rows = [r for r in ws.call("fauna.bridges.list_account_aliases", {})["aliases"]
                if r["pattern"] == pattern]
    assert _placed(nest_instance["db_path"], actor_id) == before
    assert len(rows) == 1 and rows[0]["disabled"] is True, (
        f"the switched-off alias must be kept, marked disabled; rows={rows}")


@pytest.mark.feature("mail-aliases")
def test_deleted_alias_unknown_or_falls_to_catch_all(mail_bridge_mta, nest_instance):
    """A deleted address is refused `550 User unknown`; once the admin names a
    catch-all for the domain, the same address falls through to it instead
    (§ Don't do these — the fall-through is the designed drop semantics)."""
    mta = mail_bridge_mta
    db_path = nest_instance["db_path"]
    user, _, _ = _fresh_mailbox(nest_instance, mta.domain)
    _, catch_actor, _ = _fresh_mailbox(nest_instance, mta.domain)
    pattern = f"gone{secrets.token_hex(3)}"
    with _user_ws(nest_instance, user) as ws:
        alias_id = _create_alias(ws, mta.domain, pattern)
        ws.call("fauna.bridges.delete_account_alias", {"alias_id": alias_id})
    address = f"{pattern}@{mta.domain}"
    _send(mta, address, expect_rcpt="550", expect_reply="User unknown")

    with _admin_ws(nest_instance) as admin:
        admin.call("fauna.bridges.set_catch_all_actor",
                   {"domain": mta.domain, "actor_id": catch_actor})
        try:
            before = _placed(db_path, catch_actor)
            _send(mta, address)
            assert _placed(db_path, catch_actor) == before + 1, (
                "with a catch-all set, a deleted address must deliver to it")
        finally:
            # The fixture's domain is session-shared: never leave a catch-all
            # behind for a later test expecting `User unknown`.
            admin.call("fauna.bridges.set_catch_all_actor",
                       {"domain": mta.domain, "actor_id": None})
