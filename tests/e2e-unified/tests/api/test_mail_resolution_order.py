"""tier_3 API e2e: one fixed resolution order for every address the nest hosts.

`docs/features/mail-aliases.md` outcomes 31, 32 and 33, observed from outside
(`docs/goal/behavior/mail-aliases.md` § Resolution order at RCPT TO time — the
handle match ruled 2026-10-15, the one-resolver ruling 2026-08-18):

* **31** — mail to a handle, with or without a `+suffix`, reaches its owner with
  no alias row — on the venue's second domain, where mail-enable wrote no
  canonical row — ahead of another account's wildcard and the domain's
  catch-all; on the primary domain a handle that spells another account's
  throwaway address takes that address's mail, and the throwaway is not
  spent. The owner sends from `<handle>+<tag>` through the app's door.
* **32** — exact > `+suffix` > disposable > wildcard > catch-all > `550 User
  unknown`, each step shown winning against the one below it; an empty
  sub-address answers `550 Invalid sub-address`; and only an exact identity
  logs in — a `+suffix`, wildcard or catch-all address is refused at
  submission `AUTH`, while the login address itself is admitted.
* **33** — the app's own send (`fauna.email.send`) to the nest's plus,
  wildcard, catch-all and disposable addresses delivers like mail from outside:
  the disposable's count is spent, and a capped address over its cap is not
  delivered locally (the recipient is handed to the outbound queue, to meet the
  same verdict at the nest's own perimeter — § Per-alias rate-cap, *Second
  consumer*).

Deliveries are counted in `bridge_imap_messages` (the row IMAP reads). The MTA
commits ingest before it ACKs the DATA dot, refuses at RCPT synchronously, and
`fauna.email.send` delivers in-domain copies before it replies, so every
assertion reads settled state (convention 14).

The domain's catch-all is session-shared state: every test that sets one clears
it in a `finally`.
"""

from __future__ import annotations

import secrets
import sqlite3
import time

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register, mint_and_hold_token, register_user
from helpers.budgets import MAIL_OUTBOUND_CYCLE_S
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _connect_submission_tls,
    _find_tagged,
    _poke_outbound_bridge,
    _read_final_reply,
)
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


def _mailbox(nest, handle: str | None = None) -> dict:
    """A fresh account with a recipient seal key (the MTA seals to it), so mail
    to it can be placed. ``handle`` admits it under that handle; by default the
    harness's own `e2e-<12 hex>`. Publishing the key is mail-enable, which
    writes the canonical exact row on the PRIMARY domain only."""
    if handle is None:
        actor = create_actor_and_register(
            nest["port"], admin_signing_key=nest["admin"]["signing_key"])
    else:
        sk = SigningKey.generate()
        register_user(nest["port"], bytes(sk.verify_key).hex(),
                      admin_signing_key=nest["admin"]["signing_key"], handle=handle)
        mint_and_hold_token(nest["url"], sk)
        actor = {"signing_key": sk, "actor_id_bytes": bytes(sk.verify_key),
                 "actor_id_hex": bytes(sk.verify_key).hex(), "handle": handle}
    with _admin_ws(nest) as admin:
        provision_recipient_seal_key(admin, actor["actor_id_bytes"])
    return actor


def _placed(nest, actor) -> int:
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        return conn.execute(
            "SELECT COUNT(*) FROM bridge_imap_messages WHERE actor_id = ?",
            (actor["actor_id_bytes"],),
        ).fetchone()[0]
    finally:
        conn.close()


class _Counts:
    """Placed-message counts of several accounts, read before and after."""

    def __init__(self, nest, **actors):
        self._nest, self._actors = nest, actors
        self._before = {k: _placed(nest, a) for k, a in actors.items()}

    def gained(self) -> dict:
        return {k: _placed(self._nest, a) - self._before[k] for k, a in self._actors.items()}


def _alias(client, domain, pattern, kind="exact", **controls):
    return client.call("fauna.bridges.create_account_alias", {
        "kind": kind, "local_domain": domain, "pattern": pattern,
        "controls": {"label": "", **controls},
    })["alias_id"]


def _alias_row(client, alias_id) -> dict:
    rows = [r for r in client.call("fauna.bridges.list_account_aliases", {})["aliases"]
            if r["alias_id"] == alias_id]
    assert len(rows) == 1, f"alias {alias_id!r} must be listed once: {rows!r}"
    return rows[0]


def _deliver(mta, rcpt, expect_rcpt="250", expect_reply=None) -> None:
    """One inbound transaction at the MX to ``rcpt``."""
    deadline = time.monotonic() + 30.0
    token = secrets.token_hex(6)
    body = (
        f"From: <peer@{OUTSIDE}>\r\nTo: <{rcpt}>\r\nSubject: order {token}\r\n"
        f"Message-ID: <{token}@{OUTSIDE}>\r\nDate: Mon, 25 May 2026 12:00:00 +0000\r\n"
        f"\r\nresolution order {token}\r\n"
    ).encode()
    with _connect_smtp_starttls(mta.mx_port, mta.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<peer@{OUTSIDE}>", "250", deadline)
        reply = conn.cmd(f"RCPT TO:<{rcpt}>", expect_rcpt, deadline)
        if expect_rcpt == "250":
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(body)
            conn.cmd(".", "250", deadline)
        elif expect_reply is not None:
            assert expect_reply in reply, (
                f"RCPT {rcpt} refusal must say {expect_reply!r}; got {reply!r}")
        conn.cmd("QUIT", "221", deadline)


def _app_send(nest, sender, from_addr, rcpt, token=None) -> dict:
    token = token or secrets.token_hex(6)
    raw = (
        f"From: <{from_addr}>\r\nTo: <{rcpt}>\r\nSubject: app send {token}\r\n"
        f"Message-ID: <{token}@{from_addr.split('@', 1)[1]}>\r\n"
        "Date: Mon, 25 May 2026 12:00:00 +0000\r\n"
        "MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n"
        f"\r\napp send {token}\r\n"
    ).encode()
    with _ws(nest, sender) as ws:
        return ws.call("fauna.email.send", {"recipients": [rcpt], "raw_rfc5322": raw})


class _CatchAll:
    """Designate ``actor`` the catch-all of ``domain`` for a block, then clear it."""

    def __init__(self, nest, domain, actor):
        self._nest, self._domain, self._actor = nest, domain, actor

    def _set(self, actor_id):
        with _admin_ws(self._nest) as admin:
            admin.call("fauna.bridges.set_catch_all_actor",
                       {"domain": self._domain, "actor_id": actor_id})

    def __enter__(self):
        self._set(self._actor["actor_id_bytes"])
        return self

    def __exit__(self, *_exc):
        self._set(None)


@pytest.mark.feature("mail-aliases")
def test_mail_to_a_handle_reaches_its_owner_with_no_alias_row_ahead_of_every_pattern(
    nest_instance, mail_bridge_mta,
):
    nest, mta = nest_instance, mail_bridge_mta
    second = mta.second_domain
    tag = secrets.token_hex(3)
    owner = _mailbox(nest, handle=f"rh{tag}-owner")
    rival = _mailbox(nest)
    catcher = _mailbox(nest)
    handle = owner["handle"]

    with _ws(nest, owner) as ws:
        rows = [r for r in ws.call("fauna.bridges.list_account_aliases", {})["aliases"]
                if r["local_domain"] == second]
    assert rows == [], f"the owner must hold no alias on {second}: {rows!r}"

    # Another account's wildcard covers the handle on the second domain; the
    # control shows the wildcard itself is live there.
    with _ws(nest, rival) as ws:
        wildcard_id = _alias(ws, second, f"rh{tag}-", kind="wildcard_prefix")
    try:
        counts = _Counts(nest, owner=owner, rival=rival, catcher=catcher)
        with _CatchAll(nest, second, catcher):
            _deliver(mta, f"{handle}@{second}")
            _deliver(mta, f"{handle}+receipts@{second}")
            _deliver(mta, f"rh{tag}-shop@{second}")
        assert counts.gained() == {"owner": 2, "rival": 1, "catcher": 0}, (
            "the handle and its +suffix must reach the owner ahead of the wildcard and "
            f"the catch-all, the wildcard keeping its other addresses: {counts.gained()}")
    finally:
        with _ws(nest, rival) as ws:
            ws.call("fauna.bridges.delete_account_alias", {"alias_id": wildcard_id})

    # A handle that spells another account's throwaway address takes its mail,
    # and the throwaway's count is not spent.
    with _ws(nest, rival) as ws:
        throwaway = ws.call("fauna.bridges.generate_disposable_alias", {"uses": 5})
    spelled = _mailbox(nest, handle=throwaway["full_address"].split("@", 1)[0])
    counts = _Counts(nest, spelled=spelled, rival=rival)
    _deliver(mta, throwaway["full_address"])
    assert counts.gained() == {"spelled": 1, "rival": 0}, (
        f"the handle must win over the throwaway it spells: {counts.gained()}")
    with _ws(nest, rival) as ws:
        assert _alias_row(ws, throwaway["alias_id"])["uses_remaining"] == 5, (
            "mail the handle took must not spend the throwaway")

    # The owner sends as <handle>+<tag> through the app's door.
    token = f"plusfrom{secrets.token_hex(5)}"
    _app_send(nest, owner, f"{handle}+news@{second}", f"{token}@{OUTSIDE}", token)
    _poke_outbound_bridge(mta.nest_url)
    received = wait_until(
        lambda: _find_tagged(mta.stub_mx, token), MAIL_OUTBOUND_CYCLE_S, interval=0.5,
        diagnose=lambda: f"the send from {handle}+news@{second} never reached the outside "
        f"server (bridge log: {mta.log_file})",
    )
    assert f"{handle}+news@{second}".encode() in received, (
        "the relayed message must still be from the +suffix address")


def _auth(mta, username: str, secret: str) -> str:
    """Submission AUTH PLAIN as ``username``; returns the final reply line."""
    import base64

    deadline = time.monotonic() + 30.0
    ir = base64.b64encode(b"\x00" + username.encode() + b"\x00" + secret.encode()).decode()
    with _connect_submission_tls(mta.submission_port_465, mta.domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {mta.domain}", "250", deadline)
        conn._sock.sendall(f"AUTH PLAIN {ir}\r\n".encode())
        return _read_final_reply(conn, deadline)


@pytest.mark.feature("mail-aliases")
def test_addresses_resolve_in_one_fixed_order_and_only_an_exact_identity_logs_in(
    nest_instance, mail_bridge_mta,
):
    nest, mta = nest_instance, mail_bridge_mta
    domain = mta.domain
    tag = secrets.token_hex(3)
    wide = _mailbox(nest)      # holds the wildcard that covers everything below
    exact = _mailbox(nest)     # holds the exact addresses under that wildcard
    spender = _mailbox(nest)   # holds a throwaway the wildcard also covers
    catcher = _mailbox(nest)

    with _ws(nest, wide) as ws:
        _alias(ws, domain, f"ox{tag}-", kind="wildcard_prefix")
    with _ws(nest, exact) as ws:
        _alias(ws, domain, f"ox{tag}-shop")
        _alias(ws, domain, f"ox{tag}-pb")
    with _ws(nest, spender) as ws:
        throwaway = ws.call("fauna.bridges.generate_disposable_alias", {"uses": 2})
    # A second wildcard, the spender's local part, also covers the throwaway.
    shadow = _mailbox(nest)
    with _ws(nest, shadow) as ws:
        _alias(ws, domain, f"{spender['handle']}-", kind="wildcard_prefix")

    counts = _Counts(nest, wide=wide, exact=exact, spender=spender, shadow=shadow,
                     catcher=catcher)
    _deliver(mta, f"ox{tag}-shop@{domain}")             # exact over wildcard
    _deliver(mta, f"ox{tag}-pb+tag@{domain}")           # +suffix over wildcard
    _deliver(mta, throwaway["full_address"])             # disposable over wildcard
    assert counts.gained() == {"wide": 0, "exact": 2, "spender": 1, "shadow": 0,
                               "catcher": 0}, counts.gained()
    with _ws(nest, spender) as ws:
        assert _alias_row(ws, throwaway["alias_id"])["uses_remaining"] == 1, (
            "the throwaway's delivery must spend one use")

    counts = _Counts(nest, wide=wide, catcher=catcher)
    unknown = f"nobody{tag}@{domain}"
    with _CatchAll(nest, domain, catcher):
        _deliver(mta, f"ox{tag}-anything@{domain}")      # wildcard over catch-all
        _deliver(mta, unknown)                           # catch-all over a refusal
    assert counts.gained() == {"wide": 1, "catcher": 1}, counts.gained()
    _deliver(mta, unknown, expect_rcpt="550", expect_reply="User unknown")
    _deliver(mta, f"ox{tag}-pb+@{domain}", expect_rcpt="550",
             expect_reply="Invalid sub-address")

    # Only an exact identity logs in. The venue's submission sender holds a
    # credential; its login address is the control.
    sender = mta.submission_sender_actor
    secret = mta.submission_credential.decode()
    login = f"{mta.submission_sender_local}@{domain}"
    assert _auth(mta, login, secret).startswith("235"), "the login address must log in"
    with _ws(nest, sender) as ws:
        sender_wildcard = _alias(ws, domain, f"snd{tag}-", kind="wildcard_prefix")
    try:
        with _CatchAll(nest, domain, sender):
            for address in (f"{mta.submission_sender_local}+tag@{domain}",
                            f"snd{tag}-x@{domain}", f"loose{tag}@{domain}"):
                reply = _auth(mta, address, secret)
                assert reply.startswith("5"), (
                    f"{address} resolves to the sender but is no login; AUTH said {reply!r}")
    finally:
        with _ws(nest, sender) as ws:
            ws.call("fauna.bridges.delete_account_alias", {"alias_id": sender_wildcard})


@pytest.mark.feature("mail-aliases")
def test_an_app_send_to_the_nests_own_addresses_delivers_like_outside_mail(
    nest_instance, mail_bridge_mta,
):
    nest, mta = nest_instance, mail_bridge_mta
    domain = mta.domain
    tag = secrets.token_hex(3)
    sender = _mailbox(nest)
    owner = _mailbox(nest)
    catcher = _mailbox(nest)
    from_addr = f"{sender['handle']}@{domain}"

    with _ws(nest, owner) as ws:
        _alias(ws, domain, f"as{tag}-", kind="wildcard_prefix", rate_limit_per_hour=1)
        throwaway = ws.call("fauna.bridges.generate_disposable_alias", {"uses": 3})

    counts = _Counts(nest, owner=owner, catcher=catcher)
    with _CatchAll(nest, domain, catcher):
        sent = [
            _app_send(nest, sender, from_addr, f"{owner['handle']}+app@{domain}"),
            _app_send(nest, sender, from_addr, f"as{tag}-first@{domain}"),
            _app_send(nest, sender, from_addr, throwaway["full_address"]),
            _app_send(nest, sender, from_addr, f"stray{tag}@{domain}"),
        ]
    assert [r["local_delivered"] for r in sent] == [1, 1, 1, 1], (
        f"each of the nest's own addresses must be delivered in-domain: {sent!r}")
    assert counts.gained() == {"owner": 3, "catcher": 1}, counts.gained()
    with _ws(nest, owner) as ws:
        assert _alias_row(ws, throwaway["alias_id"])["uses_remaining"] == 2, (
            "the app's delivery to the throwaway must spend one use")

    # The wildcard's cap of one an hour is spent: the next send is not
    # delivered locally — the recipient goes to the outbound queue, to meet the
    # same verdict at the nest's own perimeter.
    counts = _Counts(nest, owner=owner)
    over = _app_send(nest, sender, from_addr, f"as{tag}-second@{domain}")
    assert over["local_delivered"] == 0 and over["remote_queued"] == 1, (
        f"a capped address over its cap must not be delivered in-domain: {over!r}")
    assert counts.gained() == {"owner": 0}, counts.gained()
