"""The guardian's hold on a child's mail, observed from outside — full-stack (tier_3) API E2E.

Witnesses `docs/features/family-safety.md` outcome **61** — *"A hold on the
child's mail outranks the child's own filter rules and the spam verdict, held
mail stays held after the setting is relaxed, mail to the child's sub-addresses
is gated like mail to their plain address, and system mail is never gated"*
(`docs/goal/behavior/family-safety.md` § The mail gate; the sub-address arm
also `docs/goal/behavior/mail-aliases.md` § Kind 2).

Mail arrives the way a stranger's does: a real SMTP transaction into the real
Go MTA bridge (`mail_bridge_mta`, no docker), which seals it to the child and
hands it to the nest's sealed-ingest core, where the nest recomputes the
guardian's verdict from stored policy (`bridge_routing_handlers.rs`, the
guardian mail gate). Where a message lands is read from the row an IMAP SELECT
reads (`bridge_imap_messages.mailbox`), the same placement read the filter and
spam witnesses use (`tests/test_mail_bridge_filter_delivery.py`,
`tests/test_mail_policy_effects.py`); the guardian's queue is read over
`fauna.family.approvals.list`. The fixture's fake rspamd scores a message by
its `X-Test-Rspamd-Score` header — the one way a test makes the shared rules
call a message spam.

The outside-observed twins of `bins/fauna-nest/tests/conformance_family_mail_gate.rs`
(`cold_mail_to_a_hold_ward_lands_in_the_held_mailbox_never_inbox`,
`holds_drain_even_after_the_guardian_relaxes_the_knob`) and
`email_handlers.rs::send_refuses_a_supervised_ward_reached_at_a_subaddress`;
the outrank and system-mail arms have no conformance pin of their own (the
system-mail decision is unit-tested in `fauna_core::data`), so these are their
first full-stack witnesses.

Every arm's discriminator is the knob itself: the same mail with the setting at
`allow` lands where the child's rule, the spam verdict or the sub-address would
put it — so "held" is the guardian's hold, never a delivery that goes nowhere.

Each test admits its own guardian and child (admin-admitted, no anonymous
budget spent) and uses sender domains unique to the run, so placements are
counted per sender, never assumed from an empty mailbox.

Process safety: no ``pkill``/``killall``; the MTA bridge and the nest are
session fixtures torn down by their own fixtures.
"""

from __future__ import annotations

import os
import sqlite3
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.admin_wire import admin_client, admit_adult, admit_ward_directly
from helpers.family_wire import actor_ws, set_ward_policy
from helpers.mail_aliases import add_exact_alias
from helpers.mail_wire import _connect_smtp_starttls
from helpers.recipient_seal_key import provision_recipient_seal_key
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

#: `bridge_imap::GUARDIAN_HELD_MAILBOX` — the child's held mailbox.
HELD = "Guardian Review"

#: One SMTP transaction plus the nest's placement, and a scheduled action's
#: security notice reaching the child's mailbox. Generous; a green poll returns
#: the moment the row is there.
PLACEMENT_S = 30.0

#: A combined score of 8 on the fixture's fake rspamd (the bridge scales the
#: header by the catalog's 0.5): above the default junk threshold of 5.
SPAM_RSPAMD_SCORE = 16


def _tag() -> str:
    return os.urandom(3).hex()


def _mail_ward(nest, mta) -> tuple[dict, dict, str]:
    """A guardian and a mail-receiving child: the child holds an address on the
    MTA's domain (its own write) and a published seal key. Returns
    ``(guardian, ward, ward_local_part)``."""
    guardian = admit_adult(nest, f"famgate-guardian-{_tag()}")
    local = f"famgate-ward-{_tag()}"
    ward = admit_ward_directly(nest, guardian["actor_id_hex"], local)
    add_exact_alias(nest["url"], ward["signing_key"], mta.domain, local)
    admin_sk = nest["admin"]["signing_key"]
    with WsRpcAdminClient(
        nest["url"], actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    ) as admin_ws:
        provision_recipient_seal_key(admin_ws, bytes.fromhex(ward["actor_id_hex"]))
    return guardian, ward, local


def _deliver(mta, mail_from: str, rcpt_to: str, *, rspamd_score: float | None = None) -> None:
    """One inbound SMTP transaction through the real MTA (STARTTLS on 25),
    asserting the 250 on the DATA dot — the nest's ingest committed. A loopback
    peer is exempt from the sender-domain MX check, so the sender domain need
    not resolve."""
    deadline = time.monotonic() + 40.0
    lines = [
        f"From: Sender <{mail_from}>",
        f"To: {rcpt_to}",
        f"Subject: mail gate {_tag()}",
        f"Message-ID: <{time.time_ns()}.{_tag()}@{mail_from.split('@', 1)[1]}>",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
    ]
    if rspamd_score is not None:
        lines.append(f"X-Test-Rspamd-Score: {rspamd_score}")
    body = "\r\n".join([*lines, "", "the child's to read"]) + "\r\n"
    with _connect_smtp_starttls(mta.mx_port, mta.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{mail_from}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt_to}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _placements(nest, ward: dict) -> list[tuple[str, str]]:
    """Every ``(sender_domain, mailbox)`` the nest holds for the child — the row
    an IMAP SELECT reads (`from_norm` is the lowercased sender domain)."""
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        return conn.execute(
            "SELECT from_norm, mailbox FROM bridge_imap_messages WHERE actor_id = ?1",
            (bytes.fromhex(ward["actor_id_hex"]),),
        ).fetchall()
    finally:
        conn.close()


def _mailboxes_of(nest, ward: dict, sender_domain: str) -> list[str]:
    return sorted(m for (d, m) in _placements(nest, ward) if d == sender_domain)


def _await_mailboxes(nest, ward: dict, sender_domain: str, want: list[str], what: str) -> None:
    """Wait until the child's mail from ``sender_domain`` sits in exactly
    ``want`` (one entry per message) — the placement is the observable."""
    wait_until(
        lambda: _mailboxes_of(nest, ward, sender_domain) == sorted(want),
        PLACEMENT_S,
        diagnose=lambda: (
            f"{what}: mail from {sender_domain} sits in "
            f"{_mailboxes_of(nest, ward, sender_domain)}, expected {sorted(want)}"
        ),
    )


def _held_entries(nest, guardian: dict, ward: dict) -> list[dict]:
    """The guardian's `mail_hold` queue entries for this child."""
    want = bytes.fromhex(ward["actor_id_hex"])
    with actor_ws(nest, guardian) as ws:
        approvals = ws.call("fauna.family.approvals.list", {}).get("approvals") or []
    return [
        a
        for a in approvals
        if a.get("kind") == "mail_hold" and bytes(a.get("supervised_actor_id") or b"") == want
    ]


@pytest.mark.feature("family-safety")
def test_a_hold_outranks_the_childs_filter_rule_and_the_spam_verdict(
    nest_instance, mail_bridge_mta
):
    """The child files one sender into a folder of their own with a filter rule;
    a second sender's mail is scored as spam. Under `hold` both land in the held
    mailbox — the rule is never consulted and the spam disposition never places
    it, or the message would be one rule away from escaping the hold. With the
    setting at `allow` the same two senders' mail lands in the child's folder
    and in Junk, which is what shows the rule and the verdict were live."""
    mta = mail_bridge_mta
    guardian, ward, local = _mail_ward(nest_instance, mta)
    rcpt = f"{local}@{mta.domain}"
    ruled = f"ruled-{_tag()}.test"
    spammy = f"spammy-{_tag()}.test"

    with actor_ws(nest_instance, ward) as ws:
        ws.call(
            "fauna.email.filters.create",
            {
                "name": "file this sender",
                "rules": [{"SenderDomain": {"domain": ruled}}],
                "combination": "all",
                "action": {"FileInto": {"mailbox": "Kept"}},
                "priority": 0,
            },
        )

    set_ward_policy(nest_instance, guardian, ward, unknown_sender_mail="hold")
    _deliver(mta, f"someone@{ruled}", rcpt)
    _deliver(mta, f"someone@{spammy}", rcpt, rspamd_score=SPAM_RSPAMD_SCORE)
    _await_mailboxes(nest_instance, ward, ruled, [HELD],
                     "under hold, mail the child's rule would file must be held")
    _await_mailboxes(nest_instance, ward, spammy, [HELD],
                     "under hold, mail scored as spam must be held, never filed to Junk")
    assert len(_held_entries(nest_instance, guardian, ward)) == 2, (
        "both held messages must wait in the guardian's queue"
    )

    set_ward_policy(nest_instance, guardian, ward, unknown_sender_mail="allow")
    _deliver(mta, f"someone@{ruled}", rcpt)
    _deliver(mta, f"someone@{spammy}", rcpt, rspamd_score=SPAM_RSPAMD_SCORE)
    _await_mailboxes(nest_instance, ward, ruled, ["Kept", HELD],
                     "with the hold off the child's rule must file the same sender's mail")
    _await_mailboxes(nest_instance, ward, spammy, ["Junk", HELD],
                     "with the hold off the spam verdict must file the same sender's mail to Junk")


@pytest.mark.feature("family-safety")
def test_held_mail_stays_held_after_relaxing_and_sub_addresses_are_gated(
    nest_instance, mail_bridge_mta
):
    """A stranger mails the child's plain address and a sub-address of it
    (`<local>+news@`); both are held. The guardian then relaxes the setting to
    `allow`: both messages are still in the held mailbox and still in the
    guardian's queue, and approving one releases it to INBOX — relaxing changes
    what arrives next, never what is already held. The next sub-address
    message, under `allow`, reaches INBOX: the sub-address resolves to the child,
    so the earlier hold was the gate's."""
    mta = mail_bridge_mta
    guardian, ward, local = _mail_ward(nest_instance, mta)
    plain = f"plain-{_tag()}.test"
    sub = f"sub-{_tag()}.test"

    set_ward_policy(nest_instance, guardian, ward, unknown_sender_mail="hold")
    _deliver(mta, f"stranger@{plain}", f"{local}@{mta.domain}")
    _deliver(mta, f"stranger@{sub}", f"{local}+news@{mta.domain}")
    _await_mailboxes(nest_instance, ward, plain, [HELD], "mail to the plain address is held")
    _await_mailboxes(nest_instance, ward, sub, [HELD],
                     "mail to a sub-address must be gated like the plain address")
    held = _held_entries(nest_instance, guardian, ward)
    assert len(held) == 2, f"both holds wait in the guardian's queue: {held!r}"

    set_ward_policy(nest_instance, guardian, ward, unknown_sender_mail="allow")
    assert _mailboxes_of(nest_instance, ward, plain) == [HELD], (
        "relaxing the setting must leave the plain-address hold in the held mailbox"
    )
    assert _mailboxes_of(nest_instance, ward, sub) == [HELD], (
        "relaxing the setting must leave the sub-address hold in the held mailbox"
    )
    still = _held_entries(nest_instance, guardian, ward)
    assert len(still) == 2, f"relaxing must not drop the guardian's queue: {still!r}"

    plain_entry = next(
        (e for e in still if (e.get("peer_address") or "").endswith(f"@{plain}")), None
    )
    assert plain_entry is not None, f"the plain-address hold names its sender: {still!r}"
    with actor_ws(nest_instance, guardian) as ws:
        ws.call(
            "fauna.family.approvals.decide",
            {
                "supervised_actor_id": bytes.fromhex(ward["actor_id_hex"]),
                "kind": "mail_hold",
                # Required on every decide; a mail hold names no actor, so it
                # carries the entry's own (empty) value — the hold is keyed on
                # `message_id`, never the address.
                "peer_actor_id": bytes(plain_entry.get("peer_actor_id") or b""),
                "message_id": bytes(plain_entry["message_id"]),
                "approve": True,
            },
        )
    _await_mailboxes(nest_instance, ward, plain, ["INBOX"],
                     "a hold decided after relaxing is still the guardian's to release")
    assert _mailboxes_of(nest_instance, ward, sub) == [HELD], (
        "releasing one hold must not sweep the other"
    )

    _deliver(mta, f"stranger@{sub}", f"{local}+news@{mta.domain}")
    _await_mailboxes(nest_instance, ward, sub, ["INBOX", HELD],
                     "under allow the sub-address must deliver to the child's INBOX")


@pytest.mark.feature("family-safety")
def test_system_mail_is_never_held(nest_instance, mail_bridge_mta):
    """With the hold on, a stranger's mail is held — and the nest's own security
    notice is not. The admin schedules the child's account for deletion, which
    the nest tells the child by a security notice it writes into the child's
    mailbox itself (`security_notify.rs`, `MailIngress::System`); it lands in
    INBOX, and nothing new joins the held mailbox or the guardian's queue. The
    scheduled deletion is then cancelled, which leaves the account as it was."""
    mta = mail_bridge_mta
    guardian, ward, local = _mail_ward(nest_instance, mta)
    stranger = f"stranger-{_tag()}.test"

    set_ward_policy(nest_instance, guardian, ward, unknown_sender_mail="hold")
    _deliver(mta, f"someone@{stranger}", f"{local}@{mta.domain}")
    _await_mailboxes(nest_instance, ward, stranger, [HELD],
                     "the hold is live: a stranger's mail is held")
    held_before = len(_held_entries(nest_instance, guardian, ward))

    def _count(mailbox: str) -> int:
        return sum(1 for (_d, m) in _placements(nest_instance, ward) if m == mailbox)

    inbox_before, held_mailbox_before = _count("INBOX"), _count(HELD)

    with admin_client(nest_instance) as admin:
        scheduled = admin.call(
            "fauna.admin.users.delete",
            {"actor_id": bytes.fromhex(ward["actor_id_hex"])},
        )
    action_id = scheduled.get("pending_action_id")
    assert isinstance(action_id, int), f"the deletion is scheduled, not immediate: {scheduled!r}"
    try:
        wait_until(
            lambda: _count("INBOX") > inbox_before,
            PLACEMENT_S,
            diagnose=lambda: (
                f"the nest's security notice never reached the child's INBOX: "
                f"placements {_placements(nest_instance, ward)}"
            ),
        )
        assert _count(HELD) == held_mailbox_before, (
            f"the nest's own notice must never be held: placements "
            f"{_placements(nest_instance, ward)}"
        )
        assert len(_held_entries(nest_instance, guardian, ward)) == held_before, (
            "the nest's own notice must never reach the guardian's queue"
        )
    finally:
        with admin_client(nest_instance) as admin:
            admin.call("fauna.pending_actions.cancel", {"id": action_id})
